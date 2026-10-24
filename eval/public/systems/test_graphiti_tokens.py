"""Offline check of actual Graphiti request kwargs; no model, server, or database calls."""
import os
import sys
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from systems import graphiti as adapter
from graphiti_core.prompts.models import Message
from pydantic import BaseModel


class Reply(BaseModel):
    ok: bool


class GraphStub:
    def __init__(self, *args, **kwargs):
        self.llm = kwargs["llm_client"]

    async def build_indices_and_constraints(self):
        pass

    async def close(self):
        pass


with patch.object(adapter, "Graphiti", GraphStub), patch.object(adapter, "llm", return_value=("http://stub.invalid/v1", "stub", "local")), patch.object(adapter, "OpenAIEmbedder", return_value=None), patch.object(adapter, "OpenAIRerankerClient", return_value=None):
    system = adapter.GraphitiSystem()
    client = system.g.llm
    system.loop.run_until_complete(client.client.close())
    requests = []

    async def create(**kwargs):
        requests.append(kwargs)
        return SimpleNamespace(choices=[SimpleNamespace(message=SimpleNamespace(content='{"ok":true}'))])

    client.client = SimpleNamespace(chat=SimpleNamespace(completions=SimpleNamespace(create=create)))
    for requested in [None, 16384, 1024]:
        kwargs = {} if requested is None else {"max_tokens": requested}
        result = system.loop.run_until_complete(client.generate_response([Message(role="user", content="Extract facts.")], response_model=Reply, model="model-override", **kwargs))
        assert result == {"ok": True}
    system.close()

actual = [request["max_tokens"] for request in requests]
assert actual == [8192, 8192, 1024], actual
assert all(request["model"] == "model-override" for request in requests)
assert all(request["response_format"]["type"] == "json_schema" for request in requests)
print("PASS: default, explicit edge-extraction and smaller limits; model and JSON schema preserved")
