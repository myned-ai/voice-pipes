"""Reference voice-pipes agent (Python, gRPC).

A minimal but complete implementation of the AgentService protocol
(voice-pipes/proto/agent.proto) demonstrating every capability the
pipeline brokers:

- turn replies streamed as TurnFragments (the "brain" role on classic
  pipelines, mounted via `[[text_processor]] type = "agent-grpc"`);
- the smart-filler flow: a slow request gets an immediate filler, the
  turn ENDS, the work continues in the background, and the result
  arrives later as an out-of-turn Say (survives the user chatting in
  between — this agent's own state tracks the pending task);
- cancel-by-words while a task is pending;
- idle re-engagement wording (UserIdle -> Say);
- tool backend for S2S sessions (ListTools + ToolCall -> ToolResult,
  including the Pending ack + CompleteTool late result);
- tool transparency Messages and history upkeep from ContextNote /
  Interrupted frames.

There is no LLM here — replies are canned so the protocol mechanics
stay legible. Swap `reply_to()` for your model of choice.

Setup (once):
    pip install grpcio grpcio-tools
    python -m grpc_tools.protoc -I ../../voice-pipes/proto \
        --python_out=. --grpc_python_out=. agent.proto

Run:
    python reference_agent.py [--port 50061]

Then point a worker at it:
    [agent]                       # S2S: agent = tool backend
    endpoint = "http://127.0.0.1:50061"

    [[text_processor]]            # classic: agent = the brain
    type = "agent-grpc"
    endpoint = "http://127.0.0.1:50061"
"""

import argparse
import json
import queue
import threading
import time
from concurrent import futures

import grpc

import agent_pb2
import agent_pb2_grpc

PROTOCOL_VERSION = 1


class SessionState:
    """Per-session agent state: history + pending background tasks."""

    def __init__(self, session_id: str):
        self.session_id = session_id
        self.history = []  # (role, text)
        self.pending_tasks = {}  # task_id -> description

    def record(self, role: str, text: str) -> None:
        self.history.append((role, text))


def out_frame(**kwargs) -> agent_pb2.AgentToPipeline:
    return agent_pb2.AgentToPipeline(**kwargs)


class ReferenceAgent(agent_pb2_grpc.AgentServiceServicer):
    # ── S2S tool backend ────────────────────────────────────
    def ListTools(self, request, context):
        return agent_pb2.ListToolsResponse(
            tools=[
                agent_pb2.ToolSpec(
                    name="check_order",
                    description="Look up the status of an order by id.",
                    parameters_json_schema=json.dumps(
                        {
                            "type": "object",
                            "properties": {"order_id": {"type": "string"}},
                            "required": ["order_id"],
                        }
                    ),
                )
            ]
        )

    # ── The per-session brain link ──────────────────────────
    def AgentSession(self, request_iterator, context):
        # Flush response headers immediately. tonic (and most gRPC
        # clients) await them when opening the stream; Python's grpc
        # server otherwise defers headers until the first yield, and we
        # yield nothing until SessionStart arrives -- a mutual wait.
        context.send_initial_metadata(())

        outbox: "queue.Queue[agent_pb2.AgentToPipeline]" = queue.Queue()
        state = SessionState("?")

        def emit(frame):
            outbox.put(frame)

        def reader():
            for frame in request_iterator:
                handle_frame(frame, state, emit)
            outbox.put(None)  # pipeline closed the stream

        threading.Thread(target=reader, daemon=True).start()

        while True:
            frame = outbox.get()
            if frame is None:
                return
            yield frame


def handle_frame(frame, state: SessionState, emit) -> None:
    kind = frame.WhichOneof("event")

    if kind == "session_start":
        s = frame.session_start
        state.session_id = s.session_id
        print(f"[{state.session_id}] session start (v{s.protocol_version})")
        emit(
            out_frame(
                session_ack=agent_pb2.SessionAck(
                    protocol_version=PROTOCOL_VERSION,
                    agent_name="reference-agent",
                )
            )
        )

    elif kind == "user_turn":
        turn = frame.user_turn
        state.record("system" if turn.is_context else "user", turn.text)
        reply_to(turn, state, emit)

    elif kind == "interrupted":
        i = frame.interrupted
        # Barge-in ground truth: keep only what the user heard.
        if state.history and state.history[-1][0] == "assistant":
            state.history[-1] = ("assistant", i.played_text)
        print(f"[{state.session_id}] turn {i.turn_id} truncated to: {i.played_text!r}")

    elif kind == "context_note":
        n = frame.context_note
        role = "assistant" if n.kind == "assistant_message" else "system"
        state.record(role, n.text)

    elif kind == "user_idle":
        idle = frame.user_idle
        # The agent owns the wording of re-engagement.
        wording = (
            "Are you still with me?"
            if idle.prompt_count == 1
            else "I'll be here whenever you're ready."
        )
        emit(
            out_frame(
                say=agent_pb2.Say(
                    text=wording, scheduling=agent_pb2.SCHEDULING_WHEN_IDLE
                )
            )
        )

    elif kind == "tool_call":
        call = frame.tool_call
        print(f"[{state.session_id}] tool call {call.name}({call.arguments_json})")
        # Ack immediately (the provider model speaks the filler in its
        # own voice), deliver the real result in the background.
        emit(
            out_frame(
                tool_result=agent_pb2.ToolResult(
                    call_id=call.call_id,
                    pending=True,
                    output=json.dumps(
                        {
                            "status": "working",
                            "instruction": "briefly tell the user you're checking",
                        }
                    ),
                )
            )
        )

        def finish_tool():
            time.sleep(2.0)  # the "slow backend"
            emit(
                out_frame(
                    complete_tool=agent_pb2.CompleteTool(
                        call_id=call.call_id,
                        result="Order 7 shipped yesterday, arriving Thursday.",
                        scheduling=agent_pb2.SCHEDULING_WHEN_IDLE,
                    )
                )
            )

        threading.Thread(target=finish_tool, daemon=True).start()

    elif kind == "session_end":
        print(f"[{state.session_id}] session end: {frame.session_end.reason}")


def reply_to(turn, state: SessionState, emit) -> None:
    """The 'brain'. Canned logic keeps the protocol mechanics legible."""
    text = turn.text.lower()

    def fragment(t, end=False):
        emit(
            out_frame(
                turn_fragment=agent_pb2.TurnFragment(
                    turn_id=turn.turn_id, text=t, is_end_of_response=end
                )
            )
        )

    # Cancel-by-words while a task is pending.
    if state.pending_tasks and any(w in text for w in ("cancel", "never mind", "stop")):
        task_id, desc = state.pending_tasks.popitem()
        reply = f"Okay, I've cancelled {desc}."
        state.record("assistant", reply)
        fragment(reply, end=True)
        return

    # Interim turn while a task is pending: stay aware of it.
    if state.pending_tasks and not turn.is_context:
        desc = next(iter(state.pending_tasks.values()))
        reply = f"Still working on {desc} — I'll let you know the moment it's done."
        state.record("assistant", reply)
        fragment(reply, end=True)
        return

    # The smart-filler flow: filler now, turn ends, result later.
    if "look up" in text or "check" in text:
        task_id = f"task-{len(state.history)}"
        state.pending_tasks[task_id] = "that lookup"
        # Tool transparency chip for rich clients (activity, not transcript).
        emit(
            out_frame(
                message=agent_pb2.Message(
                    kind="tool_call",
                    payload_json=json.dumps({"name": "lookup", "arguments": {}}),
                    record_in_history=False,
                )
            )
        )
        filler = "Sure — let me look into that. One moment."
        state.record("assistant", filler)
        fragment(filler, end=True)  # the turn ENDS; user may keep talking

        def finish():
            time.sleep(3.0)  # the slow work
            if task_id not in state.pending_tasks:
                return  # cancelled meanwhile
            del state.pending_tasks[task_id]
            emit(
                out_frame(
                    say=agent_pb2.Say(
                        text="Good news — I found what you asked about: everything checks out.",
                        scheduling=agent_pb2.SCHEDULING_WHEN_IDLE,
                    )
                )
            )

        threading.Thread(target=finish, daemon=True).start()
        return

    # Context turns (agent Generate input / tool results).
    if turn.is_context:
        reply = f"Quick update: {turn.text}"
        state.record("assistant", reply)
        fragment(reply, end=True)
        return

    # Default: stream a two-fragment canned reply.
    state.record("assistant", "Hello! I heard you. How can I help?")
    fragment("Hello! I heard you. ")
    fragment("How can I help?", end=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=50061)
    args = parser.parse_args()

    server = grpc.server(futures.ThreadPoolExecutor(max_workers=16))
    agent_pb2_grpc.add_AgentServiceServicer_to_server(ReferenceAgent(), server)
    server.add_insecure_port(f"[::]:{args.port}")
    server.start()
    print(f"reference-agent listening on :{args.port}")
    server.wait_for_termination()


if __name__ == "__main__":
    main()
