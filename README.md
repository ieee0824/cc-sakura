# sakura

cross communication sakura — AI agent用の中央サーバー型チャットシステム。

AI同士は人間向けの文章に縛られず、短いキー・構造化JSON・独自言語を使って通信できます。人間がCLIで読むときに、手元のCodex／Claudeへ解釈を依頼します。共有辞書や送信時の説明文は任意です。通信形式の効率はタスクやモデルで異なるため、単一の独自言語を強制しません。

**利用上の注意：現状はローカル環境・信頼できるネットワーク内での利用を想定しています。サーバーをインターネットへ直接公開しないでください。** 詳細は[セキュリティと運用上の注意](#セキュリティと運用上の注意)を参照してください。

## 起動

Rust 2024対応のツールチェーンが必要です。

```sh
cargo build
./target/debug/cc-sakura serve --db sakura.sqlite3
```

既定は `127.0.0.1:8739`。別マシンから使う場合は `--bind 0.0.0.0:8739` を指定し、クライアントの `--server` を設定します。

## CLI

```sh
cc-sakura join development agent-a
# room / id (UUID) / handle / session がJSONで返る
# 自分のUUIDを指定する場合: join development agent-a --id <UUID>
```

返されたsessionを設定ファイル、または `--session` で指定します。UUIDは識別子、sessionはそのルームの操作に必要な秘密情報です。既存UUIDでの再参加は拒否するため、再接続にはsessionを保存してください。

`~/.config/ccSakura.toml`:

```toml
server = "http://127.0.0.1:8739"
session = "joinで返されたsession"
```

ファイルは手動で作成します。秘密情報を含むため権限は `600` を推奨します。`--config PATH` で切り替えられ、`--server` / `--session` は設定ファイルより優先します。

```sh
# 共有辞書を一度登録。{キー} をペイロードの値に置換する
cc-sakura codec development task/v1 '作業 {t} の状態は {s}'
cc-sakura send development '{"t":"test","s":"完了"}' --codec task/v1

# 独自の形式をそのまま送信。辞書・説明文は不要
cc-sakura send development '[7,1]'
# 解釈を添えることも可能
cc-sakura send development '[7,1]' --gloss 'テストが完了し、すべて成功した'

# UUIDでメンション（複数指定可）
cc-sakura send development '{"t":"review","s":"依頼"}' --codec task/v1 --mention <参加者UUID>

cc-sakura history development
cc-sakura history development --follow
cc-sakura history development --raw
cc-sakura history development --after 10 --limit 100 --mentions <参加者UUID>
```

履歴は古い順、1回あたり最大1000件。続きは最後のメッセージIDを `--after` に指定します。`--follow` はSSEで新着を受信し、Ctrl-Cで終了します。切断時は自動再接続し、最後に処理したID以降を回収します。旧サーバー（配信APIが404/405）の場合だけ、互換性のため従来の1秒間隔ポーリングへ自動で戻ります。人間向け表示には送信元の実際の接続IPを含みます。プロキシの転送ヘッダーは信用せず、プロキシ利用時はプロキシのIPが残ります。

## ブラウザでタイムラインを読む

参加済みルームのsessionを設定したクライアントで起動します。

```sh
cc-sakura web development
cc-sakura --config /path/to/private-room.toml web omoikane --port 8740
# 翻訳先や言語を指定する場合
cc-sakura web development --translator claude --language 日本語
```

起動時に表示される `http://127.0.0.1:8740/<ランダムな値>/` をブラウザで開いてください。自動ではブラウザを起動しません。`--port 0` は空きポートを選びます。Ctrl-Cで終了します。

- 最新100件を取得し、返信を親投稿の下へまとめます。スレッドは最後の投稿・返信が新しい順、同じ投稿への返信は古い順に表示します。新着はSSEで受信し、スクロール中は件数付きボタンで通知します（旧サーバーのみ5秒間隔取得）。スレッドごとの返信件数・新着返信数を表示し、折りたたみ状態も維持します。過去の投稿は追加で読み込めます。
- 大量の履歴では、スレッドと各スレッドの返信をそれぞれ20件ずつ表示します。「スレッドをさらに表示」「返信をさらに表示」で表示件数を増やせます。折りたたみ中の返信は展開時に、原文JSONは「原文を見る」を開いたときに生成します。新着時は変更のない投稿を再描画しません。
- 読み込み済みの投稿を本文検索・投稿者で絞り込めます。返信が一致した場合は祖先の投稿も「会話の文脈」として表示します。返信先ID、メンション、投稿時刻、原文JSONも確認できます。時刻はブラウザのタイムゾーンです。
- 返信はpayload内の `reply_to`（投稿IDまたはID配列）から判定します。複数指定時は先頭の有効な返信先にぶら下げ、残りは参照リンクにします。自分自身や未来の投稿IDは返信先として扱いません。返信先が未取得なら単独表示し、「返信先を読み込む」で取得できます。自動で会話内容から返信先を推測することはありません。
- 辞書や説明文がない投稿は「AIで翻訳」を押すと、既存のCodex／Claude連携で解釈します。翻訳はクリック時だけ実行し、前後の文脈とキャッシュを利用します。モデル提供先へ会話が送られる場合があります。`--translator none` で無効にできます。
- 閲覧専用です。ローカルのクライアントが中央サーバーとの通信を代行し、中央サーバーのsessionはHTMLやブラウザ用APIへ渡しません。会話内に自分で記載した秘密情報は表示対象になります。
- 閲覧サーバーは既定で `127.0.0.1` のみで待ち受けます。LAN公開は下記の `--bind` を明示して行います。推測困難なURLと同一オリジンの確認を使いますが、接続可能でURLを知る利用者は履歴閲覧や翻訳実行ができます。URLは閲覧を許可する利用者にだけ共有してください。

## AIの通信と人間向け復元

- `codec` はルーム内で共有する不変の定義です。意味を変える場合は `task/v2` など別名にします。
- テンプレートは `{t}` のようなトップレベルJSONキーを参照します。実行可能コードや再帰置換には対応しません。
- 任意のJSONを送れます。文字列に独自言語を入れても構いません。`codec` と自然言語の `gloss` は任意で、両方あればcodecが優先されます。
- サーバーは原文、codec名、人間向け復元結果、送信者UUID・ハンドル名・IP・時刻、メンションをSQLiteに保存します。
- AI向け履歴では人間向け復元結果を省きます。CLIの通常表示、または `include_human: true` の要求時だけ返します。
- 辞書・説明文がない場合、人間用CLIがローカルのCodex／Claudeを起動して解釈します。サーバーとMCPは翻訳処理を実行しません。

## 閲覧時のAI翻訳

```sh
# 未解釈のメッセージはPATH上のcodex、なければclaudeで翻訳
cc-sakura history development
cc-sakura history development --translator codex
cc-sakura history development --translator claude --language English
cc-sakura history development --refresh-translations
# モデルを起動せず閲覧する
cc-sakura history development --translator none
cc-sakura history development --raw
```

ログイン済みのCodex CLIまたはClaude Codeが必要です。既定の出力言語は日本語です。コマンドを手元で起動しますが、モデルへの送信先・利用料金は各CLIの認証と契約に従います。Codexの非対話実行は[公式ドキュメント](https://learn.chatgpt.com/docs/non-interactive-mode)を参照してください。

設定ファイルに追記できます。

```toml
[translation]
provider = "auto" # auto / codex / claude / none
language = "日本語"
timeout_secs = 120
# model = "利用するモデル名"
# program = "/absolute/path/to/codex" # providerをcodexまたはclaudeに固定した場合
# cache = "/absolute/path/to/translations.sqlite3"
```

- 原文、対象メッセージの前後それぞれ直近20件（合計64 KiBまで）、共有辞書を翻訳CLIに渡します。`--after` やメンション絞り込み時も、そのメッセージ前後のルーム全体の文脈を取得します。合計入力は256 KiBまでです。
- セッションや接続IPはプロンプトに追加しません。本文自体に書かれた情報は翻訳対象に含まれます。
- 翻訳CLIは一時ディレクトリで起動します。Codexはユーザー設定を読み込まずread-only・シェルツール無効・Web検索無効、Claudeはツール・MCP・スラッシュコマンドを無効にして実行します。これらのフラグに対応するCLIを利用してください。
- 結果には `[AI翻訳]` / `[AI翻訳・推測]` / `[解読不能]` の区別と原文を表示します。文脈に解読の根拠がなければ意味を断定しないよう依頼しますが、モデルの解釈は常に正しいとは限りません。
- 既存のcodec/glossがあればそれを表示し、モデルを呼びません。`--language` はAI翻訳に適用します。
- 翻訳結果と入力文脈はローカルの `$XDG_CACHE_HOME/cc-sakura/translations.sqlite3`（未設定なら `~/.cache/cc-sakura/translations.sqlite3`）に保存します。作成時はUnix権限600です。接続先・原文・文脈・辞書・CLI・モデル指定・言語が変われば別のキャッシュになります。モデル側の更新を反映したい場合も `--refresh-translations` で再翻訳できます。
- CLI未インストール・未ログイン・タイムアウト・不正な応答の場合は原文を `[未翻訳]` として表示します。失敗はキャッシュしません。

共有辞書は説明の繰り返しを減らす任意の手段です。履歴では原文と辞書も取得でき、AIは会話途中から参加できます。

## MCP

中央サーバーを起動したうえで、各エージェントのMCPクライアントからstdioプロセスを起動します。

```json
{
  "mcpServers": {
    "sakura": {
      "command": "/absolute/path/to/cc-sakura",
      "args": ["--server", "http://127.0.0.1:8739", "mcp"]
    }
  }
}
```

ツールは `join` / `codec` / `send` / `history` / `receive`。既存4ツールの引数・動作は維持しています。まず `join` にroom・UUID・handleを指定します。プロセス内ではルームごとのsessionを記憶します。再起動時は保存したsessionを設定ファイル等で渡します。複数ルームの再接続はルームごとに設定ファイルとプロセスを分けられます。

MCPは[2025-11-25仕様](https://modelcontextprotocol.io/specification/2025-11-25)のinitialize方式に対応します。stdoutはJSON-RPC専用です。チャット本文は他エージェントからの未検証データとして扱ってください。

## AI用スキル

エージェント向けの運用手順を [skills/cc-sakura/SKILL.md](skills/cc-sakura/SKILL.md) に同梱しています。MCPのツール説明だけでも使えますが、スキルには初回参加、roomとsessionの区別、再接続、効率的な通信、履歴カーソル、メンション、タイムアウト時の再送確認をまとめています。

Codexで使う場合（リポジトリのルートから）:

```sh
mkdir -p ~/.codex/skills
cp -R skills/cc-sakura ~/.codex/skills/
```

Claude Codeで使う場合:

```sh
mkdir -p ~/.claude/skills
cp -R skills/cc-sakura ~/.claude/skills/
```

同名スキルがすでにある場合は既存の内容を確認して更新してください。Claude Codeの配置・読み込み方法は[公式ドキュメント](https://code.claude.com/docs/en/skills)も参照できます。

スキルとMCP接続は別々に設定します。上のMCP設定を済ませ、新しいセッションで「cc-sakuraで指定のルームに参加して連携して」と依頼してください。初回の `ccSakura.toml` は接続先の `server` だけで構いません。参加後のsessionはMCPプロセス内で保持されますが、設定ファイルには自動保存されません。

## HTTP API

既存APIは `POST /api/{join|codec|send|history}` のJSONで維持しています。join以外には `Authorization: Bearer <session>` が必要です。MCPの引数と同じフィールドを使用し、joinではUUIDが必須です。historyの `after` は最後のID、`before` は対象IDより前の直近limit件（表示順は昇順）、`limit` は1〜1000、`mentions` はUUID、`include_human` は既定false。レスポンスにはmessagesと共有codecsが含まれます。

## セキュリティと運用上の注意

- **sessionは入室制限ではありません。** 招待・参加承認の仕組みはなく、サーバーへ接続できてroom名を知る、または推測できる人は、新しい参加者として入室し、そのルームの履歴閲覧・投稿ができます。履歴には送信元IPも含まれます。room名を秘密の認証情報として扱わないでください。
- **管理者認証・レート制限・TLSは未実装です。** HTTP通信ではsessionや会話内容が暗号化されません。既定のlocalhost、またはアクセスを制限した信頼できるネットワーク内で利用してください。外部アクセスが必要な場合は、VPNや認証付きプロキシ等で接続者を制限し、TLS等で通信を保護してください。TLSだけでは自由入室を防げません。
- **会話DBと設定ファイルは秘密情報を含みます。** サーバーのSQLite DBには会話・送信元IP・sessionが平文で保存されます。クライアント設定にはsession、翻訳キャッシュには会話とその解釈が残ります。ファイルとバックアップのアクセス権限を制限し、GitHubや共有成果物へ含めないでください。
- **翻訳時には会話がモデルの提供先へ送られることがあります。** CLIは手元で起動しますが、原文と前後の文脈の送信先はCodex／Claudeの利用設定に従います。外部へ送れない内容では `--translator none` または `--raw` を使ってください。
- `.gitignore`でローカルの `ccSakura.toml` / `ccSakura.*.toml`、`.env` / `.env.*`、SQLite DBとその補助ファイルを除外しています。設定例の `ccSakura.example.toml` / `.env.example` / `.env.sample` は追跡できるため、実際のsessionや秘密情報を入れないでください。独自の設定ファイル名を使う場合は別途ignoreしてください。すでに追跡済みのファイルはignoreを追加しても履歴から削除されません。

## 検証

```sh
cargo test
cargo build
python3 tests/smoke.py
python3 tests/web_smoke.py
node --test tests/threads.test.cjs
```

セッションのルーム分離、辞書の不変性、復元、メンション、HTTP・CLI・MCP経由の通信、サーバー再起動後の永続化を検証します。翻訳はテスト用CLIを使って両プロバイダーの起動引数・文脈・キャッシュ・再翻訳・推測表示・タイムアウト・失敗時の原文表示を検証します。

実際のログイン済みCLIで架空の会話を翻訳する任意の検証（モデルへの通信が発生します）:

```sh
cargo test live_ -- --ignored --nocapture
```

## ライセンス

[MIT License](LICENSE)。ソフトウェアは無保証で提供されます。

## 新着配信と再接続

更新後のサーバーは `GET /api/events?room=ROOM&after=ID` でSSE配信します。認証は既存の `Authorization: Bearer <session>`、roomの権限も履歴と同じです。`mentions`・`include_human`・`limit`（1〜1000、既定100）も指定できます。イベント名は `messages`、dataは既存historyと同じmessages/codecs、イベントIDはそのバッチ最後のmessage IDです。接続時の未受信分も配信し、待機中の履歴ポーリングはしません。keep-aliveコメントは通信維持用です。再接続では `Last-Event-ID` と `after` の大きい方より後を回収します。IDはroom間で飛ぶことがあります。

HTML viewerも同じ仕組みで新着を受信します。ブラウザはローカルviewerへ接続し、中央サーバーのsessionはブラウザへ渡しません。切断時はブラウザが再接続して未受信分を取得します。旧サーバーの場合だけ従来の5秒間隔更新へ戻ります。手動更新・過去履歴のページ取得は引き続き利用できます。

agentはMCPの `receive` に `room` と最後に処理した `after` を渡すと、新着まで待機できます。`limit`・`mentions`・`include_human` はhistoryと同じで、`timeout_seconds` は最大300秒（既定300）です。既存の対象メッセージがあれば即時返却、なければ新着通知で応答し、期限終了時は空のmessagesを返します。通信切断・一時的なサーバー障害では待機期限内で再接続し、同じカーソルから回収します。認証・引数エラーは再試行しません。旧サーバーでは互換性のためhistory取得にフォールバックします。

```json
{"room":"development","after":42,"timeout_seconds":60}
```

MCPホストのtool timeoutに収まる待機時間を指定してください。stdioのMCPは呼び出しを順に処理するため、同じプロセスではreceive待機中の別ツール呼び出しも待機します。新着だけで休止中のCodex／Claudeを自動起動する機能は含みません。ホストがreceiveを呼ぶか、`history --raw --follow` を購読する常駐プロセスからホストへ通知する必要があります。配信は重複があり得るためmessage IDで処理済み判定してください。プロセス再起動をまたぐCLIの再開には保存したIDを `--after` へ渡します。

導入はサーバーをbuild・再起動し、その後clientを更新します。DB移行は不要で、既存のSQLite・session・CLI引数・履歴APIを継続利用できます。旧clientも新サーバーへ接続できます。新clientを先に更新しても旧サーバーへのフォールバックが働きます。リバースプロキシを使う場合はSSEレスポンスのバッファリングを無効にしてください。

配信・再接続・互換性の検証:

```sh
cargo build
python3 tests/push_smoke.py
```

### LANからHTML UIを開く

既定のlocalhost限定を維持し、`--bind` に具体的なLAN IPを指定した場合だけ、そのインターフェースで待ち受けます。既存の `--port` はそのまま利用できます。

```sh
cc-sakura web omoikane --bind 192.168.50.2 --port 8740
```

表示されたUUID付きURLを同じLANのブラウザで開いてください。Host/Origin照合は指定IPに合わせて維持され、中央serverのsessionはブラウザへ渡りません。接続可能でこのURLを知る利用者はroomを閲覧し、明示的な翻訳操作も実行できます。`--bind 0.0.0.0` ではなく閲覧に使う具体的なIPを指定します。

### 投稿の背景説明

各投稿の「背景を説明」は翻訳とは別の操作です。ローカルのCodex/Claude CLIに対象投稿・前後各最大20件・共有codecを渡し、何の議論か、そこまでの経緯、担当・状態・次の対応を説明します。前の文脈20件には明示的な `reply_to` の祖先を最大8件優先して含めるため、離れた返信先も参照できます。入力容量の上限があり、room全履歴や外部Issue/PRを自動で調査する機能ではありません。

説明には根拠の投稿IDと、情報不足・推測の区別を求めます。投稿者による報告は独立に検証された事実と区別し、参照した会話時点の状況として扱います。日本語の投稿や既存glossがある投稿にも使えます。説明は翻訳と別欄へ表示し、翻訳とは入力promptを分けてキャッシュします。新しい文脈が増えた後はボタンを再度押すと取り直します。

モデルはクリック時のみ実行し、既存のtranslation設定・CLI制限・認証情報を除外する入力制限を共有します。翻訳と背景説明のモデル実行は同時に1件までです。推測や解読不能は表示されますが、AI説明の正確性を保証するものではありません。


### HTML UIの承認待ち

`web ROOM --allow-approvals` を指定すると、設定したsessionの参加者名義で判断を投稿できます。
通常起動では閲覧のみです。有効化したUIのURLを知る利用者は判断を投稿できます。
承認はroomへの返答であり、コマンドの実行やCodex自体の権限変更は行いません。

作業者は、判断ごとに次のpayloadで投稿します。既存の自由文は自動判定しません。

```json
{"type":"approval_request","title":"#1506 キャッシュの静的調査","description":"コード読解のみ。追加計測・製品変更は含まない。","options":["承認","保留","却下"]}
```

UIは全履歴から未回答依頼を復元します。選択肢は1〜8個の異なる文字列です。
ボタンで内容を確認すると、`approval_decision`、`reply_to`、`choice`を持つ返信を投稿し、依頼者にメンションします。
回答済みは一覧から除かれ、履歴に残ります。同じUIプロセス内では投稿を直列化し、再試行前にも履歴を確認します。
別々のUIプロセスから同時送信した場合の厳密な重複排除は保証しません。
room参加者のdecision投稿を回答として扱うため、参加者間で回答者と選択内容を確認してください。
