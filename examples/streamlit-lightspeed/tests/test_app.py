from pathlib import Path

import pytest
from streamlit.testing.v1 import AppTest

import demo
from demo import Chat, Rpc, Turn
from lightspeed_ai import LightspeedTransportError

APP = Path(__file__).resolve().parents[1] / "app.py"


class Gateway:
    def __init__(self, _settings):
        pass

    def identify_universe(self):
        return "dev"

    def submit(self, chat, turn):
        self.submissions.append((chat.session_id, turn.submission_id, turn.prompt))
        chat.opened = True
        if self.lose_response and len(self.submissions) == 1:
            raise LightspeedTransportError("lost reply")
        turn.run_id = str(len(chat.turns))
        turn.status = "queued"
        turn.calls.extend([
            Rpc("session/runs/start", "submit", 1.0, "HTTP 200", 10),
            Rpc("session/events/read", "poll", 2.0, "HTTP 200", 20),
        ])

    def poll(self, _chat, turn):
        turn.status = "completed"
        turn.detail = {"id": turn.run_id, "status": "completed", "outputText": f"Reply to {turn.prompt}"}

    def open_session(self, session_id):
        turn = Turn("Earlier prompt", run_id="1", status="completed",
                    detail={"id": "1", "status": "completed", "outputText": "Stored reply"})
        return Chat(session_id, opened=True, turns=[turn])


@pytest.fixture
def app(monkeypatch):
    Gateway.submissions = []
    Gateway.lose_response = False
    monkeypatch.setenv("LIGHTSPEED_API_KEY", "test-key")
    monkeypatch.setenv("LIGHTSPEED_DEMO_TEMPORAL_ADDRESS", "127.0.0.1:7233")
    monkeypatch.setattr(demo, "Gateway", Gateway)
    return AppTest.from_file(str(APP), default_timeout=15)


def test_chat_follow_ups_and_reopen(app):
    app.run()
    app.chat_input[0].set_value("First").run()
    assert not app.exception
    assert any(item.value == "Reply to First" for item in app.markdown)
    app.chat_input[0].set_value("Follow-up").run()
    assert not app.exception
    assert any(item.value == "Reply to Follow-up" for item in app.markdown)
    assert Gateway.submissions[0][0] == Gateway.submissions[1][0]
    assert Gateway.submissions[0][1] != Gateway.submissions[1][1]
    app.text_input(key="session_to_open").set_value("saved-session")
    next(button for button in app.button if button.label == "Open session").click().run()
    assert not app.exception
    assert any(item.value == "Stored reply" for item in app.markdown)


def test_retry_reuses_submission_and_inspection_failure_leaves_chat_available(app, monkeypatch):
    Gateway.lose_response = True

    async def fail(*_args):
        raise RuntimeError("connection detail")

    monkeypatch.setattr(demo, "temporal_history", fail)
    app.run()
    app.chat_input[0].set_value("Retry me").run()
    assert not app.exception
    app.button(key="retry_submit").click().run()
    assert not app.exception
    assert Gateway.submissions == [Gateway.submissions[0]] * 2
    app.selectbox(key="node_picker").set_value("temporal").run()
    assert not app.exception
    assert any("Evidence unavailable" in item.value for item in app.warning)
    assert not app.chat_input[0].disabled
    assert len(Gateway.submissions) == 2


def test_manual_step_uses_selected_evidence(app):
    app.run()
    app.chat_input[0].set_value("Trace this").run()
    app.toggle[0].set_value(False).run()
    assert not app.exception
    app.slider[0].set_value(1).run()
    assert not app.exception
    assert any("session/runs/start" in item.value for item in app.caption)


def test_lightspeed_link_uses_the_configured_platform_url(app, monkeypatch):
    monkeypatch.setenv("LIGHTSPEED_DEMO_PLATFORM_URL", "https://platform.example/app/")
    app.run()
    assert not app.exception
    links = app.get("link_button")
    assert any(link.proto.label == "Open Lightspeed" and link.proto.url == "https://platform.example/app/" for link in links)
