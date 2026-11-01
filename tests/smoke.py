"""End-to-end check: cargo build && python3 tests/smoke.py."""
import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid

binary = str(pathlib.Path('target/debug/cc-sakura').resolve())
with tempfile.TemporaryDirectory() as directory:
    config = pathlib.Path(directory) / 'config.toml'
    config.write_text('')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    base = f'http://127.0.0.1:{port}'
    command = [binary, '--config', str(config), '--server', base]
    server_command = command + ['serve', '--bind', f'127.0.0.1:{port}', '--db', str(pathlib.Path(directory) / 'db.sqlite')]

    def start():
        process = subprocess.Popen(server_command, stderr=subprocess.PIPE)
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    return process
            except OSError:
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                time.sleep(.05)
        process.terminate()
        raise AssertionError('server did not start')

    def api(name, args, token):
        req = urllib.request.Request(base + '/api/' + name, json.dumps(args).encode(),
            {'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
        with urllib.request.urlopen(req) as response:
            return json.load(response)

    server = start()
    try:
        member = json.loads(subprocess.check_output(command + ['join', 'lab', 'agent']))
        token = member['session']
        api('codec', {'room': 'lab', 'name': 'task/v1', 'template': '作業 {t}: {s}'}, token)
        api('send', {'room': 'lab', 'codec': 'task/v1', 'payload': {'t': 'build', 's': '成功'}, 'mentions': [member['id']]}, token)
        history = subprocess.check_output(command + ['--session', token, 'history', 'lab']).decode()
        assert '作業 build: 成功' in history and '127.0.0.1' in history
        compact = api('history', {'room': 'lab'}, token)
        assert 'human' not in compact['messages'][0]
        assert compact['messages'][0]['payload'] == {'t': 'build', 's': '成功'}
        mcp = subprocess.Popen(command + ['mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

        def rpc(message):
            mcp.stdin.write(json.dumps(message) + '\n')
            mcp.stdin.flush()
            return json.loads(mcp.stdout.readline())

        assert rpc({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {'protocolVersion': '2025-11-25', 'capabilities': {}, 'clientInfo': {'name': 'test', 'version': '1'}}})['result']['protocolVersion'] == '2025-11-25'
        mcp.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': 'notifications/initialized'}) + '\n')
        mcp.stdin.flush()
        assert len(rpc({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'})['result']['tools']) == 5
        joined = rpc({'jsonrpc': '2.0', 'id': 3, 'method': 'tools/call', 'params': {'name': 'join', 'arguments': {'room': 'lab', 'id': str(uuid.uuid4()), 'handle': 'mcp'}}})
        assert not joined['result']['isError']
        sent = rpc({'jsonrpc': '2.0', 'id': 4, 'method': 'tools/call', 'params': {'name': 'send', 'arguments': {'room': 'lab', 'payload': [1, 0], 'gloss': '完了'}}})
        assert not sent['result']['isError']
        mcp.stdin.close()
        assert mcp.wait(timeout=5) == 0
        # Translation happens only in the human CLI, with no server-side model calls.
        fake = pathlib.Path(directory) / 'translator.py'
        fake.write_text('#!' + sys.executable + '\n' + r'''import json, pathlib, sys, time
root = pathlib.Path(__file__)
prompt = sys.stdin.read()
data = json.loads(prompt.split("\n", 1)[1])
assert data["message"]["payload"] == [7, 1]
assert data["preceding_messages"][0]["payload"] == "7=テスト、1=成功"
assert "sender_ip" not in prompt and "session" not in data["message"]
assert pathlib.Path.cwd() != root.parent
with root.with_suffix('.calls').open('a') as f:
    f.write(json.dumps(sys.argv[1:]) + "\n")
mode = root.with_suffix('.mode').read_text() if root.with_suffix('.mode').exists() else 'translated'
if mode == 'fail': sys.exit(2)
if mode == 'timeout': time.sleep(3)
if mode == 'huge':
    print('x' * (1024 * 1024 + 1))
    sys.exit(0)
if mode == 'bad':
    print('invalid JSON')
    sys.exit(0)
result = {"status": mode if mode in ['uncertain', 'uninterpretable'] else 'translated', "text": "テスト成功"}
if '--print' in sys.argv:
    assert sys.argv[sys.argv.index('--tools')+1] == ''
    assert '--strict-mcp-config' in sys.argv and '--no-session-persistence' in sys.argv
    print(json.dumps({"is_error": False, "structured_output": result}))
else:
    assert '--ignore-user-config' in sys.argv and '--ephemeral' in sys.argv
    assert 'read-only' in sys.argv and 'features.shell_tool=false' in sys.argv
    print(json.dumps(result))
''')
        fake.chmod(0o700)
        config.write_text('[translation]\nprovider = "codex"\nprogram = ' + json.dumps(str(fake)) + '\ncache = ' + json.dumps(str(pathlib.Path(directory) / 'translations.sqlite3')) + '\ntimeout_secs = 1\n')
        viewer = json.loads(subprocess.check_output(command + ['join', 'translate', 'viewer']))
        session = viewer['session']
        definition = api('send', {'room': 'translate', 'payload': '7=テスト、1=成功', 'gloss': '通信辞書の定義'}, session)
        unknown = json.loads(subprocess.check_output(command + ['--session', session, 'send', 'translate', '[7,1]', '--mention', viewer['id']]))
        assert api('history', {'room': 'translate', 'include_human': True, 'after': definition['id']}, session)['messages'][0]['human'] is None
        history_cmd = command + ['--session', session, 'history', 'translate', '--after', str(definition['id']), '--mentions', viewer['id']]

        def view(*args):
            return subprocess.run(history_cmd + list(args), capture_output=True, text=True, timeout=10, check=True)

        def calls():
            return len(fake.with_suffix('.calls').read_text().splitlines())

        output = view().stdout
        assert '[AI翻訳] テスト成功' in output and '原文: [7,1]' in output
        assert calls() == 1
        assert 'テスト成功' in view().stdout and calls() == 1  # persisted cache across CLI processes
        assert 'テスト成功' in view('--refresh-translations').stdout and calls() == 2
        assert 'テスト成功' in view('--language', 'English').stdout and calls() == 3
        assert 'テスト成功' in view('--translator', 'claude').stdout and calls() == 4
        assert '[未翻訳]' in view('--translator', 'none').stdout and calls() == 4
        assert json.loads(view('--raw').stdout)['id'] == unknown['id'] and calls() == 4
        for mode, label in [('uncertain', 'AI翻訳・推測'), ('uninterpretable', '解読不能')]:
            fake.with_suffix('.mode').write_text(mode)
            assert label in view('--refresh-translations').stdout
        for mode in ['fail', 'bad', 'timeout', 'huge']:
            fake.with_suffix('.mode').write_text(mode)
            result = view('--refresh-translations')
            assert '[未翻訳] [7,1]' in result.stdout and '翻訳できませんでした' in result.stderr
        # Failed translations do not replace a successful cached interpretation.
        assert '[解読不能]' in view().stdout
        # A later explanation changes the prompt and invalidates the cached failure to decode.
        fake.with_suffix('.mode').write_text('translated')
        count_before_context_change = calls()
        api('send', {'room': 'translate', 'payload': '補足: [7,1]はテスト成功', 'gloss': '通信の補足'}, session)
        assert '[AI翻訳] テスト成功' in view().stdout
        assert calls() == count_before_context_change + 1
        assert calls() == count_before_context_change + 1 and 'テスト成功' in view().stdout
        if sys.platform != 'win32':
            assert (pathlib.Path(directory) / 'translations.sqlite3').stat().st_mode & 0o777 == 0o600
        server.terminate()
        server.wait(timeout=5)
        server = start()
        persisted = api('history', {'room': 'lab', 'include_human': True}, token)
        assert len(persisted['messages']) == 2
        assert persisted['messages'][1]['human'] == '完了'
    finally:
        server.terminate()
        server.wait(timeout=5)
print('HTTP, CLI, MCP, compact history, mentions, restart persistence, translation backends, cache, timeout, and failure fallback: OK')
