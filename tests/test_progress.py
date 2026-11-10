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
    assert "3 photos" in output or "3p" in output
    assert "100%" in output
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


def test_runtime_reports_actual_cpu_despite_other_gpu_memory_and_keeps_model_visible(monkeypatch):
    monkeypatch.setattr(progress, '_process_usage', lambda: (0, 0))
    monkeypatch.setattr(progress, '_gpu_usage', lambda: (8*1024**3,16*1024**3))
    monkeypatch.setattr(progress.shutil, 'get_terminal_size', lambda fallback: SimpleNamespace(columns=100))
    stream=InteractiveBuffer()
    display=progress.MiningProgress(stream=stream, interval=60).start()
    try:
        display.begin('embeddings', 10, 10, model='EmbeddingGemma 2', model_id='google:embeddinggemma-2:512', device='loading')
        assert 'GPU pending' in display._line()
        display.set_runtime(model='EmbeddingGemma 2', model_id='google:embeddinggemma-2:512', device='cpu')
        assert 'GPU no (CPU)' in display._line()
        assert 'EmbeddingGemma 2' in stream.getvalue().split('\r')[-1]
        assert 'GPU:no/CPU' in stream.getvalue().split('\r')[-1] or 'GPU no (CPU)' in stream.getvalue().split('\r')[-1]
        metrics=display.stage_metrics()
        assert metrics['gpu_used'] is False and metrics['device']=='cpu'
    finally:
        report=display.finish(10)
    assert report['gpu_vram_used_bytes']==8*1024**3
    assert report['gpu_used'] is False


def test_gpu_prompt_encoding_then_cpu_scoring_preserves_both_devices_and_resets_next_step():
    display=progress.MiningProgress(stream=io.StringIO())
    display.begin('labels',1,model='OpenCLIP ViT-B/32',device='loading')
    display.set_runtime(model='OpenCLIP ViT-B/32',device='cuda:0',runtime_state='encoding label prompts')
    assert 'GPU yes (cuda:0)' in display._line()
    display.set_runtime(model='OpenCLIP ViT-B/32',device='cpu',runtime_state='scoring cached vectors')
    metrics=display.stage_metrics()
    assert metrics['gpu_used'] is True
    assert metrics['devices']==['cuda:0','cpu']
    assert 'GPU no (CPU)' in display._line()
    display.begin('faces',0,model='YuNet + SFace',device='not used',runtime_state='no pending work')
    assert display.stage_metrics()['gpu_used'] is False
    assert display.stage_runtimes['labels']['gpu_used'] is True
    assert display.stage_runtimes['faces']['model']=='YuNet + SFace'


def test_empty_and_failed_model_steps_do_not_claim_a_gpu_was_used():
    display=progress.MiningProgress(stream=io.StringIO())
    display.begin('embeddings',1,model='EmbeddingGemma 2',device='loading')
    display.end_runtime()
    assert display.stage_metrics()['device']=='not used'
    assert display.stage_metrics()['gpu_used'] is False
    display.begin('embeddings',1,model='EmbeddingGemma 2',device='loading')
    display.end_runtime(failed=True)
    assert display.stage_metrics()['device']=='unknown'
    assert display.stage_metrics()['gpu_used'] is None
    assert display.stage_metrics()['runtime_state']=='failed'


def test_step_eta_shows_hours_minutes_and_resets_for_the_next_step(monkeypatch):
    clock=[0.]
    monkeypatch.setattr(progress,'time',SimpleNamespace(monotonic=lambda:clock[0]))
    monkeypatch.setattr(progress,'_process_usage',lambda:(0,0))
    display=progress.MiningProgress(stream=io.StringIO())
    display.begin('scan',total=182,pictures_total=182)
    assert display.stage_metrics()['estimated_remaining_seconds'] is None
    assert 'ETA calculating' in display._line()
    clock[0]=30.
    display.advance();display.advance()
    metrics=display.stage_metrics()
    assert metrics['estimated_remaining_seconds']==2700.
    assert metrics['estimated_remaining_minutes']==45.
    assert metrics['estimated_remaining_hours']==.75
    assert 'ETA 0h 45m' in display._line()
    display.set_total(242)
    assert 'ETA 1h 00m' in display._line()
    display.begin('embeddings',total=242,model='EmbeddingGemma 2',device='loading')
    assert display.stage_metrics()['estimated_remaining_seconds'] is None
    clock[0]=32.
    for _ in range(242):display.advance()
    assert display.stage_metrics()['estimated_remaining_seconds']==0
    assert 'ETA 0h 00m' in display._line()


def test_unknown_total_and_idle_step_never_report_false_zero_eta(monkeypatch):
    clock=[0.]
    monkeypatch.setattr(progress,'time',SimpleNamespace(monotonic=lambda:clock[0]))
    monkeypatch.setattr(progress,'_process_usage',lambda:(0,0))
    display=progress.MiningProgress(stream=io.StringIO())
    display.begin('count')
    clock[0]=2.
    display.advance();display.advance()
    assert display.stage_metrics()['estimated_remaining_seconds'] is None
    display.begin('scan',total=10)
    clock[0]=4.
    display.advance();display.advance()
    assert display.stage_metrics()['estimated_remaining_seconds']==8.
    clock[0]=65.
    assert display.stage_metrics()['estimated_remaining_seconds'] is None
    assert 'ETA calculating' in display._line()
    display.begin('faces',total=0)
    assert display.stage_metrics()['estimated_remaining_seconds']==0


def test_compact_progress_keeps_eta_model_and_actual_gpu_status_visible(monkeypatch):
    clock=[0.]
    monkeypatch.setattr(progress,'time',SimpleNamespace(monotonic=lambda:clock[0]))
    monkeypatch.setattr(progress,'_process_usage',lambda:(0,0))
    monkeypatch.setattr(progress.shutil,'get_terminal_size',lambda fallback:SimpleNamespace(columns=100))
    stream=InteractiveBuffer()
    display=progress.MiningProgress(stream=stream)
    display.begin('embeddings',30021,30021,model='EmbeddingGemma 2',device='cpu')
    clock[0]=60.
    display.advance();display.advance()
    display._render()
    line=stream.getvalue().split('\r')[-1]
    assert 'ETA 250h 10m' in line
    assert 'EmbeddingGemma 2' in line and 'GPU:no/CPU' in line
