"""Push delivery, cursor recovery, old-server fallback: cargo build && python3 tests/push_smoke.py."""
import concurrent.futures
import http.server
import json
import pathlib
import selectors
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

BINARY = str(pathlib.Path('target/debug/cc-sakura').resolve())

def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]

def event(response):
    data = []
    while True:
        line = response.readline()
        assert line, 'event stream closed'
        if line.startswith(b'data:'):
            data.append(line[5:].strip())
        if not line.strip() and data:
            return json.loads(b'\n'.join(data))

def line(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        assert selector.select(12), 'client did not receive a message'
    return json.loads(process.stdout.readline())

with tempfile.TemporaryDirectory() as directory, concurrent.futures.ThreadPoolExecutor() as pool:
    root = pathlib.Path(directory)
    config = root / 'config.toml'
    config.write_text('')
    address = port()
    base = f'http://127.0.0.1:{address}'
    common = [BINARY, '--config', str(config), '--server', base]
    server_command = common + ['serve', '--bind', f'127.0.0.1:{address}', '--db', str(root / 'db.sqlite')]
    processes = []

    def start():
        process = subprocess.Popen(server_command, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                with socket.create_connection(('127.0.0.1', address), .1):
                    return process
            except OSError:
                time.sleep(.05)
        raise AssertionError('server did not start')

    def api(operation, args, credential=None):
        headers = {'Content-Type': 'application/json'}
        if credential:
            headers['Authorization'] = 'Bearer ' + credential
        req = urllib.request.Request(base + '/api/' + operation, json.dumps(args).encode(), headers)
        with urllib.request.urlopen(req, timeout=12) as response:
            return json.load(response)

    def subscribe(credential, room='lab', after=0, **options):
        last_id = options.pop('last_id', None)
        headers = {'Authorization': 'Bearer ' + credential}
        if last_id is not None:
            headers['Last-Event-ID'] = str(last_id)
        return urllib.request.urlopen(urllib.request.Request(base + '/api/events?' + urllib.parse.urlencode({'room': room, 'after': after, **options}), headers=headers), timeout=12)

    server = start()
    try:
        member = api('join', {'room': 'lab', 'id': str(uuid.uuid4()), 'handle': 'push-agent'})
        credential = member['session']
        other = api('join', {'room': 'other', 'id': str(uuid.uuid4()), 'handle': 'other'})
        try:
            subscribe(credential, room='other')
            raise AssertionError('cross-room subscription accepted')
        except urllib.error.HTTPError as error:
            assert error.code == 400
        assert api('receive', {'room': 'lab', 'after': 0, 'timeout_seconds': 1}, credential)['messages'] == []
        with subscribe(credential) as stream:
            incoming = pool.submit(event, stream)
            time.sleep(.1)
            assert not incoming.done(), 'empty stream returned data'
            api('send', {'room': 'other', 'payload': 'private'}, other['session'])
            time.sleep(.1)
            assert not incoming.done(), 'cross-room event leaked'
            first = api('send', {'room': 'lab', 'payload': 'first'}, credential)['id']
            batch = incoming.result(5)
            assert [m['id'] for m in batch['messages']] == [first]
            assert 'human' not in batch['messages'][0]
        second = api('send', {'room': 'lab', 'payload': 'second', 'gloss': 'explanation'}, credential)['id']
        with subscribe(credential, last_id=first, include_human='true') as stream:
            batch = event(stream)
            assert [m['id'] for m in batch['messages']] == [second]
            assert batch['messages'][0]['human'] == 'explanation'
        with subscribe(credential, after=second, mentions=member['id']) as stream:
            incoming = pool.submit(event, stream)
            api('send', {'room': 'lab', 'payload': 'not mentioned'}, credential)
            mentioned = api('send', {'room': 'lab', 'payload': 'mentioned', 'mentions': [member['id']]}, credential)['id']
            assert [m['id'] for m in incoming.result(5)['messages']] == [mentioned]
        waiting = pool.submit(api, 'receive', {'room': 'lab', 'after': mentioned, 'timeout_seconds': 5}, credential)
        time.sleep(.1)
        assert not waiting.done()
        received = api('send', {'room': 'lab', 'payload': 'wake'}, credential)['id']
        assert waiting.result(5)['messages'][0]['id'] == received
        # More than one server page drains without requiring another send/wakeup.
        ids = [api('send', {'room': 'lab', 'payload': n}, credential)['id'] for n in range(105)]
        with subscribe(credential, after=received) as stream:
            batches = event(stream)['messages'] + event(stream)['messages']
            assert [m['id'] for m in batches] == ids
        follower = subprocess.Popen(common + ['--session', credential, 'history', 'lab', '--raw', '--follow', '--after', str(ids[-1])], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        processes.append(follower)
        time.sleep(.2)
        live = api('send', {'room': 'lab', 'payload': 'live'}, credential)['id']
        assert line(follower)['id'] == live
        # Restart the actual server/database; follower must reconnect and recover backlog.
        server.terminate()
        server.wait(5)
        time.sleep(.2)
        server = start()
        offline = api('send', {'room': 'lab', 'payload': 'during reconnect'}, credential)['id']
        assert line(follower)['id'] == offline
        following = api('send', {'room': 'lab', 'payload': 'after reconnect'}, credential)['id']
        assert line(follower)['id'] == following
        # MCP's extra tool blocks for a new message and reconnects within its wait budget.
        mcp = subprocess.Popen(common + ['--session', credential, 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        processes.append(mcp)
        def write(value):
            mcp.stdin.write(json.dumps(value) + '\n')
            mcp.stdin.flush()
        write({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize'})
        assert line(mcp)['result']['protocolVersion'] == '2025-11-25'
        write({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        write({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'receive', 'arguments': {'room': 'lab', 'after': following, 'timeout_seconds': 10}}})
        time.sleep(.2)
        server.terminate()
        server.wait(5)
        server = start()
        wake = api('send', {'room': 'lab', 'payload': 'mcp reconnect'}, credential)['id']
        result = line(mcp)['result']
        assert not result['isError'], result
        assert json.loads(result['content'][0]['text'])['messages'][0]['id'] == wake
        print('push / resume / isolation / batching / CLI restart / MCP reconnect: PASS')
    finally:
        for process in processes:
            process.terminate()
            process.wait(5)
        server.terminate()
        server.wait(5)

# A minimal old HTTP server proves new clients retain pull compatibility.
class Legacy(http.server.BaseHTTPRequestHandler):
    calls = []
    def log_message(self, *args):
        pass
    def do_GET(self):
        self.send_error(405)
    def do_POST(self):
        args = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.calls.append(self.path)
        value = {'error': 'unknown operation'} if self.path.endswith('/receive') else {'messages': [{'id': 1, 'payload': 'legacy'}] if args.get('after', 0) < 1 else [], 'codecs': []}
        self.send_response(400 if 'error' in value else 200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(value).encode())

with http.server.ThreadingHTTPServer(('127.0.0.1', 0), Legacy) as old, tempfile.TemporaryDirectory() as directory:
    thread = threading.Thread(target=old.serve_forever, daemon=True)
    thread.start()
    config = pathlib.Path(directory) / 'config.toml'
    config.write_text('')
    follower = subprocess.Popen([BINARY, '--config', str(config), '--server', f'http://127.0.0.1:{old.server_port}', 'history', 'lab', '--raw', '--follow'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    try:
        assert line(follower)['payload'] == 'legacy'
        assert '/api/history' in Legacy.calls
        print('legacy CLI fallback: PASS')
        mcp = subprocess.Popen([BINARY, '--config', str(config), '--server', f'http://127.0.0.1:{old.server_port}', 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        try:
            def rpc(value):
                mcp.stdin.write(json.dumps(value) + '\n')
                mcp.stdin.flush()
            rpc({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize'})
            assert 'result' in line(mcp)
            rpc({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
            rpc({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'receive', 'arguments': {'room': 'lab', 'after': 0, 'timeout_seconds': 2}}})
            result = line(mcp)['result']
            assert not result['isError'], result
            assert json.loads(result['content'][0]['text'])['messages'][0]['payload'] == 'legacy'
            assert '/api/receive' in Legacy.calls
        finally:
            mcp.terminate()
            mcp.wait(5)
        viewer = subprocess.Popen([BINARY, '--config', str(config), '--server', f'http://127.0.0.1:{old.server_port}', '--session', 'test-only', 'web', 'lab', '--port', '0'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(viewer.stdout, selectors.EVENT_READ)
                assert selector.select(10)
            url = viewer.stdout.readline().strip().removeprefix('HTML viewer: ')
            try:
                urllib.request.urlopen(url + 'api/events?after=1', timeout=5)
                raise AssertionError('old server should trigger viewer fallback')
            except urllib.error.HTTPError as error:
                assert error.code == 404
                assert json.load(error)['legacy']
        finally:
            viewer.terminate()
            viewer.wait(5)
        print('legacy MCP receive / viewer fallback: PASS')
    finally:
        follower.terminate()
        follower.wait(5)
        old.shutdown()
