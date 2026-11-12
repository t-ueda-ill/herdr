"""Re-run a workflow run that failed only because a job never got a runner.

GitHub cancels a queued job after about 15 minutes when no hosted runner
picks it up, and reports the run as failed. Nothing in the repository can
fix that, so the run is started again instead of waiting for someone to
notice. `.github/workflows/rerun-unassigned-runner.yml` calls this after
each CI run.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass

# Attempts of one run, the first included. Each attempt that gets no runner
# takes about 15 minutes to be cancelled, so this bounds the wait at 45.
MAX_RUN_ATTEMPTS = 3
NOT_ACQUIRED = "was not acquired by Runner"
PASSING_CONCLUSIONS = ("success", "skipped", "neutral")


@dataclass(frozen=True)
class Job:
    name: str
    conclusion: str | None
    runner_id: int | None
    annotations: tuple[str, ...] = ()


@dataclass(frozen=True)
class Run:
    id: int
    attempt: int
    conclusion: str | None
    jobs: tuple[Job, ...]


@dataclass(frozen=True)
class Decision:
    rerun: bool
    reason: str


def never_got_a_runner(job: Job) -> bool:
    # Not the annotation alone: a pull request's own jobs can write any
    # annotation through workflow commands, but only while on a runner.
    return not job.runner_id and any(NOT_ACQUIRED in text for text in job.annotations)


def decide(run: Run, latest_run_id: int | None) -> Decision:
    """`latest_run_id` is the newest run of the same workflow, branch and
    event; `None` when there is none."""
    if run.conclusion != "failure":
        return Decision(False, f"the run concluded {run.conclusion}, not failure")
    if run.attempt >= MAX_RUN_ATTEMPTS:
        return Decision(False, f"attempt {run.attempt} is the last one allowed")
    # A re-run joins the run's concurrency group, where CI cancels whatever is
    # in progress; an outdated run must not cancel the one that replaced it.
    if latest_run_id != run.id:
        return Decision(False, f"run {latest_run_id} replaced this one")

    unsuccessful = [
        job for job in run.jobs if job.conclusion not in PASSING_CONCLUSIONS
    ]
    unassigned = [job.name for job in unsuccessful if never_got_a_runner(job)]
    ran = [job.name for job in unsuccessful if not never_got_a_runner(job)]
    # Not "any unassigned job": the re-run would repeat these jobs too, and a
    # flaky test that passes the second time would never be looked at.
    if ran:
        return Decision(False, f"failed on a runner: {', '.join(ran)}")
    if not unassigned:
        return Decision(False, "no job is waiting for a runner")
    return Decision(True, f"never got a runner: {', '.join(unassigned)}")


def gh_api(*args: str) -> object:
    output = subprocess.run(
        ["gh", "api", *args], check=True, capture_output=True, text=True
    ).stdout
    return json.loads(output) if output.strip() else None


def annotations(repo: str, job_id: int) -> tuple[str, ...]:
    entries = gh_api(f"repos/{repo}/check-runs/{job_id}/annotations")
    return tuple(entry.get("message") or "" for entry in entries)


def fetch(repo: str, run_id: int, attempt: int) -> tuple[Run, int | None]:
    path = f"repos/{repo}/actions/runs/{run_id}/attempts/{attempt}"
    meta = gh_api(path)
    pages = gh_api(
        "--method", "GET", "--paginate", "--slurp", f"{path}/jobs", "-F", "per_page=100"
    )
    jobs = []
    for entry in (job for page in pages for job in page["jobs"]):
        name, conclusion, runner_id = (
            entry["name"],
            entry["conclusion"],
            entry["runner_id"],
        )
        # Only a job without a runner can qualify, so the rest cost no request.
        waited = conclusion not in PASSING_CONCLUSIONS and not runner_id
        notes = annotations(repo, entry["id"]) if waited else ()
        jobs.append(Job(name, conclusion, runner_id, notes))

    # The branch name comes from whoever opened the pull request, so `-f`
    # encodes it rather than this code building the query string.
    latest = gh_api(
        "--method",
        "GET",
        f"repos/{repo}/actions/workflows/{meta['workflow_id']}/runs",
        "-f",
        f"branch={meta['head_branch'] or ''}",
        "-f",
        f"event={meta['event']}",
        "-F",
        "per_page=1",
    )["workflow_runs"]
    run = Run(run_id, attempt, meta["conclusion"], tuple(jobs))
    return run, latest[0]["id"] if latest else None


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True)
    parser.add_argument("--run-id", required=True, type=int)
    parser.add_argument("--run-attempt", required=True, type=int)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)

    run, latest_run_id = fetch(args.repo, args.run_id, args.run_attempt)
    decision = decide(run, latest_run_id)
    print(f"{'rerun' if decision.rerun else 'skip'}: {decision.reason}")
    if decision.rerun and not args.dry_run:
        gh_api(
            "--method",
            "POST",
            f"repos/{args.repo}/actions/runs/{run.id}/rerun-failed-jobs",
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
