from pathlib import Path

from handoff.context.engine import ContextEngine, init_project


def test_init_project_creates_scaffold(tmp_path: Path):
    h = init_project(tmp_path)
    assert (h / "brain.md").exists()
    assert (h / "config.toml").exists()
    assert (h / "decisions").is_dir()
    assert (h / "scratch").is_dir()
    assert (h / "derived").is_dir()


def test_sync_writes_per_agent_files(tmp_path: Path):
    init_project(tmp_path)
    (tmp_path / ".handoff" / "brain.md").write_text("# brain\n\nhello\n")
    engine = ContextEngine(tmp_path)
    written = engine.sync()
    paths = {p.name for p in written}
    assert "CLAUDE.md" in paths
    assert "AGENTS.md" in paths
    assert ".cursorrules" in paths
    assert "copilot-instructions.md" in paths
    assert (tmp_path / "CLAUDE.md").read_text().startswith("# brain")


def test_sync_strips_frontmatter(tmp_path: Path):
    init_project(tmp_path)
    (tmp_path / ".handoff" / "brain.md").write_text(
        "---\ntitle: x\n---\n# brain\n\nhello\n"
    )
    ContextEngine(tmp_path).sync()
    assert (tmp_path / "CLAUDE.md").read_text().startswith("# brain")


def test_snapshot_writes_scratch(tmp_path: Path):
    init_project(tmp_path)
    engine = ContextEngine(tmp_path)
    p = engine.snapshot(note="failover from claude to codex")
    assert p.exists()
    assert "Handoff snapshot" in p.read_text()
    assert "failover from claude to codex" in p.read_text()
