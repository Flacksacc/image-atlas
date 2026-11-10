import io
from types import SimpleNamespace

from image_atlas import progress


class InteractiveBuffer(io.StringIO):
    def isatty(self):
        return True


def test_interactive_progress_replaces_one_line_and_reports_resources(monkeypatch):
    monkeypatch.setattr(progress, "_gpu_usage", lambda: (2 * 1024**3, 16 * 1024**3))
    stream = InteractiveBuffer()
    display = progress.MiningProgress(stream=stream, interval=60).start()
    display.begin("count")
    assert "COUNT [#.........] --% 0/? image paths" in display._line()
    display.begin("find", total=2, pictures_total=2)
    display.advance()
    assert "[#####-----] 50% 1/2 selected | 1 paths checked | max 2 photos" in display._line()
    display.begin("scan", 3, 3)
    for _ in range(3):
        display.advance()
    display.begin("embeddings", 2, 3)
    display.advance()
    display.advance()
    assert "images_per_second" in display.stage_metrics()
    metrics = display.finish(3)
    output = stream.getvalue()
    assert output.count("\n") == 1
    assert output.count("\r") >= 2
    assert "3 photos" in output
    assert "[##########] 100%" in output
    assert "CPU" in output and "RAM" in output and "VRAM" in output
    assert metrics["gpu_vram_used_bytes"] == 2 * 1024**3
    assert "images_per_second" in metrics
    assert metrics['images_processed'] == 3
    assert 'img/s' in output or 'i/s' in output


def test_throughput_uses_only_last_five_seconds_and_drops_when_idle(monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(progress, 'time', SimpleNamespace(monotonic=lambda: clock[0]))
    monkeypatch.setattr(progress, '_process_usage', lambda: (0, 0))
    monkeypatch.setattr(progress, '_gpu_usage', lambda: None)
    display = progress.MiningProgress(stream=InteractiveBuffer(), interval=60).start()
    try:
        display.begin('scan', 10, 10)
        assert display.stage_metrics()['images_per_second'] is None
        clock[0] = 1
        for _ in range(2):
            display.advance()
        assert display.stage_metrics()['images_per_second'] == 2.0
        assert display.stage_metrics()['throughput_window_seconds'] == 1.0
        clock[0] = 4
        for _ in range(6):
            display.advance()
        clock[0] = 6
        assert display.stage_metrics()['images_per_second'] == 1.2
        assert '1.2 img/s (5s)' in display._line()
        clock[0] = 9
        assert display.stage_metrics()['images_per_second'] == 0.0
        clock[0] = 10
        display.advance()
    finally:
        metrics = display.finish(10, cancelled=True)
    assert metrics['images_processed'] == 9
    assert metrics['images_per_second'] == 0.2
    assert metrics['throughput_window_seconds'] == 5.0
    assert metrics['throughput_stage'] == 'scan'


def test_stage_change_resets_rate_and_empty_stages_do_not_erase_final_sample(monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(progress, 'time', SimpleNamespace(monotonic=lambda: clock[0]))
    monkeypatch.setattr(progress, '_process_usage', lambda: (0, 0))
    monkeypatch.setattr(progress, '_gpu_usage', lambda: None)
    display = progress.MiningProgress(stream=InteractiveBuffer(), interval=60).start()
    try:
        display.begin('scan', 10, 10)
        clock[0] = 1
        for _ in range(10):
            display.advance()
        display.begin('embeddings', 1, 10)
        assert display.stage_metrics()['images_per_second'] is None
        clock[0] = 2
        display.advance()
        assert display.stage_metrics()['images_per_second'] == 1.0
        display.begin('faces', 0, 10)
        clock[0] = 3
    finally:
        metrics = display.finish(10)
    assert metrics['images_per_second'] == 0.5
    assert metrics['throughput_stage'] == 'embeddings'


def test_zero_elapsed_with_completed_image_does_not_divide_by_zero(monkeypatch):
    monkeypatch.setattr(progress, 'time', SimpleNamespace(monotonic=lambda: 0.0))
    monkeypatch.setattr(progress, '_process_usage', lambda: (0, 0))
    monkeypatch.setattr(progress, '_gpu_usage', lambda: None)
    display = progress.MiningProgress(stream=InteractiveBuffer(), interval=60).start()
    try:
        display.begin('scan', 1, 1)
        display.advance()
        assert display.stage_metrics()['images_per_second'] is None
    finally:
        assert display.finish(1)['images_per_second'] is None
