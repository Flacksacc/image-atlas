"""Normalize libpq options for the native viewer over private stdin/stdout."""
import json
import sys
from pathlib import Path
from psycopg.conninfo import conninfo_to_dict, make_conninfo

NATIVE_OPTIONS = {'user', 'password', 'dbname', 'host', 'hostaddr', 'port', 'options',
                  'application_name', 'sslmode', 'sslnegotiation', 'connect_timeout',
                  'keepalives', 'keepalives_idle', 'keepalives_interval', 'keepalives_count',
                  'tcp_user_timeout', 'target_session_attrs', 'channel_binding'}
TLS_OPTIONS = {'sslrootcert', 'sslcert', 'sslkey'}

def normalize(dsn):
    settings = conninfo_to_dict(dsn)
    unsupported = settings.keys() - NATIVE_OPTIONS - TLS_OPTIONS
    if unsupported:
        raise ValueError('Unsupported native connection options: ' + ', '.join(sorted(unsupported)))
    mode = settings.get('sslmode', 'prefer')
    if settings.get('sslrootcert') == 'system':
        if 'sslmode' not in settings:
            mode = 'verify-full'
        elif mode != 'verify-full':
            raise ValueError('sslrootcert=system requires verify-full')
    if mode in {'verify-full', 'verify-ca'}:
        settings['sslmode'] = 'require'
    elif mode not in {'disable', 'prefer', 'require'}:
        raise ValueError('Unsupported SSL mode for native viewer')
    root = settings.pop('sslrootcert', None)
    certificate = settings.pop('sslcert', None)
    key = settings.pop('sslkey', None)
    if bool(certificate) != bool(key):
        raise ValueError('Provide both sslcert and sslkey for a client identity')
    result = {'native_dsn': make_conninfo(**settings), 'verify_hostname': mode != 'verify-ca'}
    if root and root != 'system':
        result['root_pem'] = Path(root).read_text(encoding='ascii')
    if certificate:
        from cryptography.hazmat.primitives.serialization import load_pem_private_key, Encoding, PrivateFormat, NoEncryption
        result['certificate_pem'] = Path(certificate).read_text(encoding='ascii')
        private = load_pem_private_key(Path(key).read_bytes(), password=None)
        result['key_pem'] = private.private_bytes(Encoding.PEM, PrivateFormat.PKCS8, NoEncryption()).decode('ascii')
    return result

def main():
    try:
        result = normalize(sys.stdin.readline(65536).strip())
    except Exception as exc:
        # Values may include passwords or key contents; report only the error category.
        print('Native connection configuration failed: ' + type(exc).__name__, file=sys.stderr)
        raise SystemExit(1) from None
    print(json.dumps(result))

if __name__ == '__main__':
    main()
