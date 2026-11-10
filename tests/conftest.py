"""Run integration tests in a dedicated local database, never the app database."""
import psycopg
import pytest

from image_atlas.db import DEFAULT_DSN, connect, initialize


@pytest.fixture(autouse=True)
def isolated_database(monkeypatch, tmp_path, request):
    import uuid
    from image_atlas.catalogs import current, registry, delete_secret
    current.set(None)
    monkeypatch.setenv('IMAGE_CLI_CONFIG_DIR', str(tmp_path / 'config'))
    prefix = 'image-atlas/test/' + uuid.uuid4().hex
    monkeypatch.setenv('IMAGE_CLI_CREDENTIAL_PREFIX', prefix)
    test_dsn = DEFAULT_DSN.rsplit("/", 1)[0] + "/imagecli_test"
    admin_dsn = DEFAULT_DSN.rsplit("/", 1)[0] + "/postgres"
    with psycopg.connect(admin_dsn, autocommit=True) as admin:
        if not admin.execute("SELECT 1 FROM pg_database WHERE datname='imagecli_test'").fetchone():
            admin.execute("CREATE DATABASE imagecli_test")
    monkeypatch.setenv("IMAGE_CLI_DSN", test_dsn)
    monkeypatch.setenv("IMAGE_CLI_JOB_DIR", str(tmp_path / 'isolated-jobs'))
    initialize()
    with connect() as conn:
        conn.execute("TRUNCATE image_previews,saved_views,label_prototypes,collections,collection_settings,place_lookup_cache,sort_proposals,faces,face_groups,people,images,thumbnails RESTART IDENTITY CASCADE")
        conn.commit()
    if request.module.__name__ in {'test_mine', 'test_cloud', 'test_embedding_worker'}:
        from image_atlas.catalogs import register
        register('Test', test_dsn, [str(tmp_path)])
    yield
    current.set(None)
    for record in registry()['catalogs']:
        reference = record.get('credential')
        if reference:
            assert reference.startswith(prefix + '/')
            delete_secret(reference)
