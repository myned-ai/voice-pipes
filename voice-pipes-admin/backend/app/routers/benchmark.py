from typing import Any

import httpx
from fastapi import APIRouter, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field

from ..core.config import ORCHESTRATOR_URL, orch_client
from ..core.upstream import upstream_error, upstream_path

router = APIRouter()


# The orchestrator owns these shapes and validates them in full; what is
# checked here is the outline and the sizes, so a malformed or outsized body
# is refused before it is forwarded. Fields the orchestrator adds later pass
# through unchanged.

class ExperimentBody(BaseModel):
    model_config = ConfigDict(extra="allow")

    name: str = Field(min_length=1, max_length=200)
    description: str | None = Field(default=None, max_length=4000)
    test_suite_id: str = Field(min_length=1, max_length=200)
    variants: list[dict[str, Any]] = Field(default_factory=list, max_length=64)
    repetitions: int = Field(default=1, ge=1, le=1000)


class SuiteBody(BaseModel):
    model_config = ConfigDict(extra="allow")

    id: str | None = Field(default=None, max_length=200)
    name: str = Field(min_length=1, max_length=200)
    description: str | None = Field(default=None, max_length=4000)
    created_at: str | None = Field(default=None, max_length=64)
    scenarios: list[dict[str, Any]] = Field(default_factory=list, max_length=1000)


def _forward(body: BaseModel) -> dict:
    """The body as the browser sent it. A field it left out stays out, so
    the orchestrator applies its own default rather than this model's."""
    return body.model_dump(mode="json", exclude_unset=True)



# ── Benchmark Proxy Routes ───────────────────────────────────

@router.get("/api/v1/benchmark/configs")
async def list_benchmark_configs():
    """Proxy to orchestrator: list pipeline TOML configs available for variant selection."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/benchmark/configs", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/benchmark/experiments")
async def list_experiments():
    """Proxy to orchestrator: list benchmark experiments."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/benchmark/experiments", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.post("/api/v1/benchmark/experiments")
async def create_experiment(body: ExperimentBody):
    """Proxy to orchestrator: create a benchmark experiment."""
    async with orch_client() as client:
        try:
            response = await client.post(f"{ORCHESTRATOR_URL}/api/v1/benchmark/experiments", json=_forward(body), timeout=10.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/benchmark/experiments/{experiment_id}")
async def get_experiment(experiment_id: str):
    """Proxy to orchestrator: get a single experiment."""
    async with orch_client() as client:
        try:
            response = await client.get(ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/experiments/{experiment_id}", experiment_id=experiment_id), timeout=5.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.delete("/api/v1/benchmark/experiments/{experiment_id}")
async def delete_experiment(experiment_id: str):
    """Proxy to orchestrator: delete a benchmark experiment."""
    async with orch_client() as client:
        try:
            response = await client.delete(ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/experiments/{experiment_id}", experiment_id=experiment_id), timeout=5.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.post("/api/v1/benchmark/experiments/{experiment_id}/run")
async def run_experiment(experiment_id: str, request: Request):
    """Proxy to orchestrator: trigger experiment execution.

    Forwards query params (``speed``, ``timeout_s``, ``variants``,
    ``scenarios``, ``worker_id``) so the UI can pin a run to a specific
    registered worker or filter the variants/scenarios that run.
    """
    params = dict(request.query_params)
    async with orch_client() as client:
        try:
            response = await client.post(
                ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/experiments/{experiment_id}/run", experiment_id=experiment_id),
                params=params,
                timeout=30.0,
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/benchmark/experiments/{experiment_id}/results")
async def get_experiment_results(experiment_id: str):
    """Proxy to orchestrator: get experiment results."""
    async with orch_client() as client:
        try:
            response = await client.get(ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/experiments/{experiment_id}/results", experiment_id=experiment_id), timeout=10.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/config")
async def get_variant_config(experiment_id: str, variant_id: str):
    """Proxy to orchestrator: get frozen variant config TOML."""
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/config", experiment_id=experiment_id, variant_id=variant_id),
                timeout=5.0,
            )
            response.raise_for_status()
            from fastapi.responses import PlainTextResponse
            return PlainTextResponse(content=response.text, media_type="text/plain")
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get(
    "/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}/scenarios/{scenario_id}/entries/{entry_id}/audio"
)
async def get_tts_entry_audio(experiment_id: str, variant_id: str, scenario_id: str, entry_id: str):
    """Proxy to orchestrator: stream recorded TTS audio for a compare-table entry.

    Binary passthrough (audio/wav) — unlike the JSON routes above, errors are
    raised as HTTPExceptions because the caller is an ``<audio>`` element that
    cannot consume the ``{"data": ..., "error": ...}`` envelope.
    """
    async with orch_client() as client:
        try:
            response = await client.get(
                ORCHESTRATOR_URL + upstream_path(
                    "/api/v1/benchmark/experiments/{experiment_id}/variants/{variant_id}"
                    "/scenarios/{scenario_id}/entries/{entry_id}/audio",
                    experiment_id=experiment_id,
                    variant_id=variant_id,
                    scenario_id=scenario_id,
                    entry_id=entry_id,
                ),
                timeout=15.0,
            )
            response.raise_for_status()
            from fastapi.responses import Response as BinaryResponse
            return BinaryResponse(
                content=response.content,
                media_type=response.headers.get("content-type", "audio/wav"),
                headers={"Cache-Control": response.headers.get("cache-control", "private, max-age=60")},
            )
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:
            raise HTTPException(status_code=502, detail={"data": None, "error": upstream_error(e)}) from e


@router.get("/api/v1/benchmark/suites")
async def list_suites():
    """Proxy to orchestrator: list test suites."""
    async with orch_client() as client:
        try:
            response = await client.get(f"{ORCHESTRATOR_URL}/api/v1/benchmark/suites", timeout=5.0)
            response.raise_for_status()
            return response.json()
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.post("/api/v1/benchmark/suites")
async def create_suite(body: SuiteBody):
    """Proxy to orchestrator: create a test suite."""
    async with orch_client() as client:
        try:
            response = await client.post(f"{ORCHESTRATOR_URL}/api/v1/benchmark/suites", json=_forward(body), timeout=10.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.get("/api/v1/benchmark/suites/{suite_id}")
async def get_suite(suite_id: str):
    """Proxy to orchestrator: get a single test suite."""
    async with orch_client() as client:
        try:
            response = await client.get(ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/suites/{suite_id}", suite_id=suite_id), timeout=5.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.delete("/api/v1/benchmark/suites/{suite_id}")
async def delete_suite(suite_id: str):
    """Proxy to orchestrator: delete a test suite."""
    async with orch_client() as client:
        try:
            response = await client.delete(ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/suites/{suite_id}", suite_id=suite_id), timeout=5.0)
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}


@router.put("/api/v1/benchmark/suites/{suite_id}")
async def update_suite(suite_id: str, body: SuiteBody):
    """Proxy to orchestrator: replace an existing test suite."""
    async with orch_client() as client:
        try:
            response = await client.put(
                ORCHESTRATOR_URL + upstream_path("/api/v1/benchmark/suites/{suite_id}", suite_id=suite_id),
                json=_forward(body),
                timeout=10.0,
            )
            response.raise_for_status()
            return response.json()
        except httpx.HTTPStatusError as e:
            raise HTTPException(status_code=e.response.status_code, detail=e.response.text) from e
        except Exception as e:  # noqa: BLE001 - any upstream failure becomes the error envelope
            return {"data": None, "error": upstream_error(e)}
