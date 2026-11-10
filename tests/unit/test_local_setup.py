"""Private database setup never overwrites existing credentials."""
import importlib.util
from pathlib import Path


def load_setup():
    path = Path(__file__).resolve().parents[2] / 'scripts' / 'setup_local.py'
    spec = importlib.util.spec_from_file_location('image_atlas_setup', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_setup_creates_private_password_without_printing_it(tmp_path, capsys):
    module = load_setup()
    destination = module.setup(tmp_path)
    value = destination.read_text(encoding='utf-8').strip().split('=', 1)[1]
    assert len(value) >= 40
    assert value not in capsys.readouterr().out


def test_setup_preserves_existing_local_configuration(tmp_path, capsys):
    destination = tmp_path / '.env'
    existing = 'IMAGEATLAS_DB_PASSWORD=existing-test-value\n'
    destination.write_text(existing, encoding='utf-8')
    module = load_setup()
    module.setup(tmp_path)
    assert destination.read_text(encoding='utf-8') == existing
    assert 'preserved' in capsys.readouterr().out
