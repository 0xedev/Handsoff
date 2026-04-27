import os
from pathlib import Path

from handoff.critic.watch import WatchLoop, list_tracked_files


def test_list_tracked_files_falls_back_when_not_a_repo(tmp_path: Path):
    (tmp_path / "a.txt").write_text("a")
    (tmp_path / "b.txt").write_text("b")
    files = {p.name for p in list_tracked_files(tmp_path)}
    assert "a.txt" in files
    assert "b.txt" in files


def test_list_tracked_files_skips_handoff_dir(tmp_path: Path):
    (tmp_path / ".handoff").mkdir()
    (tmp_path / ".handoff" / "brain.md").write_text("x")
    (tmp_path / "src.py").write_text("pass")
    files = {p.name for p in list_tracked_files(tmp_path)}
    assert "src.py" in files
    assert "brain.md" not in files


def test_watch_fires_after_debounce(tmp_path: Path):
    import time as _time

    target = tmp_path / "x.py"
    target.write_text("v1")
    base = _time.time() + 100  # well into the future, > any real mtime
    os.utime(target, (base, base))

    fires: list[set[Path]] = []
    loop = WatchLoop(tmp_path, fires.append, interval=0.01, debounce=1.0)

    # tick with no changes: nothing fires
    tick = loop.tick(now=base + 1)
    assert not tick.fired
    assert fires == []

    # bump mtime; tick sees the change but debounce not elapsed
    os.utime(target, (base + 5, base + 5))
    tick = loop.tick(now=base + 5.1)
    assert target in tick.changed
    assert not tick.fired
    assert fires == []

    # another small change still within debounce
    os.utime(target, (base + 6, base + 6))
    tick = loop.tick(now=base + 6.1)
    assert not tick.fired

    # tick well after last change -> fire
    tick = loop.tick(now=base + 10)
    assert tick.fired
    assert len(fires) == 1
    assert target in fires[0]


def test_watch_does_not_fire_without_changes(tmp_path: Path):
    (tmp_path / "x").write_text("a")
    loop = WatchLoop(tmp_path, lambda _: None, interval=0.01, debounce=0.5)
    for t in [1.0, 2.0, 3.0, 4.0]:
        tick = loop.tick(now=t)
        assert not tick.fired
