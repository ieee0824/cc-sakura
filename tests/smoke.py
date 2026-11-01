"""End-to-end check: cargo build && python3 tests/smoke.py."""
import json
import pathlib
import socket
import subprocess
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
        assert len(rpc({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'})['result']['tools']) == 4
        joined = rpc({'jsonrpc': '2.0', 'id': 3, 'method': 'tools/call', 'params': {'name': 'join', 'arguments': {'room': 'lab', 'id': str(uuid.uuid4()), 'handle': 'mcp'}}})
        assert not joined['result']['isError']
        sent = rpc({'jsonrpc': '2.0', 'id': 4, 'method': 'tools/call', 'params': {'name': 'send', 'arguments': {'room': 'lab', 'payload': [1, 0], 'gloss': '完了'}}})
        assert not sent['result']['isError']
        mcp.stdin.close()
        assert mcp.wait(timeout=5) == 0
        server.terminate()
        server.wait(timeout=5)
        server = start()
        persisted = api('history', {'room': 'lab', 'include_human': True}, token)
        assert len(persisted['messages']) == 2
        assert persisted['messages'][1]['human'] == '完了'
    finally:
        server.terminate()
        server.wait(timeout=5)
print('HTTP, CLI, MCP, compact history, mentions, and restart persistence: OK')
