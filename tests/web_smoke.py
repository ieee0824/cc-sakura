"""Read-only HTML viewer E2E: cargo build && python3 tests/web_smoke.py."""
import concurrent.futures
import json
import pathlib
import selectors
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

binary = str(pathlib.Path('target/debug/cc-sakura').resolve())

def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]

def request(url, data=None, headers=None):
    req = urllib.request.Request(url, data=data, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=10) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()

with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    config = root / 'config.toml'
    fake = root / 'translator.py'
    fake.write_text('#!' + sys.executable + '\n' + r'''import json, pathlib, sys, time
root = pathlib.Path(__file__)
data = json.loads(sys.stdin.read().split('\n',1)[1])
assert data['message']['payload'] == [7,1]
assert data['preceding_messages'] and data['following_messages']
with root.with_suffix('.calls').open('a') as out: out.write('call\n')
time.sleep(.7)
print(json.dumps({'status':'uncertain','text':'テスト成功と推測します。<script>window.injected=1</script>'}))
''')
    fake.chmod(0o700)
    config.write_text('[translation]\nprovider="codex"\nprogram=' + json.dumps(str(fake)) + '\ncache=' + json.dumps(str(root / 'cache.sqlite3')) + '\n')
    port = free_port()
    base = f'http://127.0.0.1:{port}'
    common = [binary, '--config', str(config), '--server', base]
    server = subprocess.Popen(common + ['serve', '--bind', f'127.0.0.1:{port}', '--db', str(root / 'db.sqlite')], stderr=subprocess.PIPE)
    viewer = None
    try:
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1): break
            except OSError: time.sleep(.05)
        else: raise AssertionError('server did not start')

        def api(operation, args, session=None):
            headers = {'Content-Type':'application/json'}
            if session: headers['Authorization'] = 'Bearer ' + session
            status, _, body = request(base + '/api/' + operation, json.dumps(args).encode(), headers)
            assert status == 200, body
            return json.loads(body)

        joined = json.loads(subprocess.check_output(common + ['join', 'test-room', '<img src=x onerror=alert(1)>']))
        session = joined['session']
        for number in range(125):
            api('send', {'room':'test-room', 'payload':{'type':'verification_update', 'message':f'投稿 {number} <script>window.injected=1</script>'}}, session)
        target = api('send', {'room':'test-room', 'payload':[7,1]}, session)['id']
        api('send', {'room':'test-room', 'payload':'following context', 'gloss':'後続の説明'}, session)
        # Explicit session flag is test-only; viewer never includes it in its HTML/API.
        viewer = subprocess.Popen(common + ['--session', session, 'web', 'test-room', '--port', '0'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(viewer.stdout, selectors.EVENT_READ)
            assert selector.select(timeout=10), 'viewer did not print URL'
        line = viewer.stdout.readline().strip()
        assert line.startswith('HTML viewer: '), line
        url = line.removeprefix('HTML viewer: ')
        origin = '/'.join(url.split('/')[:3])
        status, headers, body = request(url)
        assert status == 200 and b'ROOM TIMELINE' in body, (status, dict(headers), body[:500])
        assert headers['Cache-Control'] == 'no-store'
        assert "script-src 'self'" in headers['Content-Security-Policy'], dict(headers)
        assert headers['X-Frame-Options'] == 'DENY'
        for asset in ['app.js', 'app.css', 'threads.js']:
            status, _, body = request(url + asset)
            assert status == 200 and session.encode() not in body
        status, _, body = request(url + 'api/info')
        assert json.loads(body)['room'] == 'test-room'
        assert session.encode() not in body and base.encode() not in body
        status, _, body = request(url + 'api/history')
        history = json.loads(body)['messages']
        assert status == 200 and len(history) == 100
        assert history[0]['id'] == 28 and history[-1]['id'] == 127
        assert session.encode() not in body
        status, _, body = request(url + 'api/message/1')
        assert status == 200 and json.loads(body)['message']['id'] == 1
        assert session.encode() not in body
        assert request(url + 'api/message/0')[0] == 400
        assert request(url + 'api/message/999999')[0] == 404
        other = api('join', {'room':'other-room','id':str(uuid.uuid4()),'handle':'other'})
        other_id = api('send', {'room':'other-room','payload':'not visible'},other['session'])['id']
        assert request(url + f'api/message/{other_id}')[0] == 404
        status, _, body = request(url + 'api/history?before=28')
        assert [m['id'] for m in json.loads(body)['messages']] == list(range(1,28))
        newer = api('send', {'room':'test-room','payload':'new post'},session)
        status, _, body = request(url + 'api/history?after=127')
        assert json.loads(body)['messages'][0]['id'] == newer['id']
        # Browser stream is a same-origin proxy: no central token is exposed.
        with urllib.request.urlopen(url + 'api/events?after=127', timeout=10) as stream:
            assert stream.headers['Content-Type'].startswith('text/event-stream')
            while True:
                line = stream.readline()
                if line.startswith(b'data:'):
                    batch = json.loads(line[5:])
                    assert batch['messages'][0]['id'] == newer['id']
                    assert session.encode() not in line
                    break
        assert request(url + 'api/events?after=127', headers={'Origin':'https://attacker.example'})[0] == 403
        assert request(url + 'api/history?after=1&before=5')[0] == 400
        assert request(origin + '/api/history')[0] == 404
        assert request(url + 'api/history', headers={'Host':'attacker.example'})[0] == 403
        assert request(url + 'api/history', headers={'Origin':'https://attacker.example'})[0] == 403
        assert request(url + 'api/history', headers={'Sec-Fetch-Site':'cross-site'})[0] == 403
        assert request(url, headers={'Sec-Fetch-Site':'cross-site','Sec-Fetch-Mode':'navigate','Sec-Fetch-Dest':'document'})[0] == 200
        assert request(url + f'api/translate/{target}', data=b'')[0] == 403
        assert not fake.with_suffix('.calls').exists(), 'viewing must not launch a model'
        # Translation must not freeze history, and another model call is rate-limited.
        action_headers = {'X-Sakura-Viewer':'1','Origin':origin}
        with concurrent.futures.ThreadPoolExecutor() as executor:
            future = executor.submit(request, url + f'api/translate/{target}', b'', action_headers)
            for _ in range(100):
                if fake.with_suffix('.calls').exists(): break
                time.sleep(.01)
            else: raise AssertionError('translator was not called')
            assert request(url + 'api/history')[0] == 200
            assert request(url + f'api/translate/{target}', b'', action_headers)[0] == 429
            status, _, translated = future.result(timeout=10)
            assert status == 200 and json.loads(translated)['status'] == 'uncertain', translated
        assert request(url + f'api/translate/{target}', b'', action_headers)[0] == 200
        assert fake.with_suffix('.calls').read_text().splitlines() == ['call'], 'cache not reused'
        # API is read-only: no proxy to join/send or arbitrary rooms.
        assert request(url + 'api/send', b'{}', action_headers)[0] == 404
        assert request(url + 'api/history?room=another-room')[0] == 200  # always uses configured room
        print('HTML assets, latest/older/new posts, local origin protection, explicit translation, cache and responsive polling: OK')
        if '--preview' in sys.argv:
            print(url, flush=True)
            try:
                while True: time.sleep(1)
            except KeyboardInterrupt: pass
    finally:
        if viewer:
            viewer.terminate()
            viewer.wait(timeout=5)
        server.terminate()
        server.wait(timeout=5)
