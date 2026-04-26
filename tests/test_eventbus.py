from handoff.daemon.eventbus import EventBus


def test_publish_calls_subscribers():
    bus = EventBus()
    received: list[dict] = []
    bus.subscribe("Foo", received.append)
    bus.publish("Foo", {"x": 1})
    bus.publish("Bar", {"x": 2})
    assert received == [{"x": 1}]


def test_listener_exception_does_not_break_bus():
    bus = EventBus()
    seen: list[dict] = []

    def boom(_p):
        raise RuntimeError("nope")

    bus.subscribe("E", boom)
    bus.subscribe("E", seen.append)
    bus.publish("E", {"ok": True})
    assert seen == [{"ok": True}]
