from __future__ import annotations

import unittest

from scripts.rerun_unassigned_runner import (
    MAX_RUN_ATTEMPTS,
    Attempt,
    Job,
    Run,
    decide,
)

NOT_ACQUIRED = (
    "The job was not acquired by Runner of type hosted even after multiple attempts"
)
CAPACITY_NOTICE = (
    "Due to capacity constraints, jobs targeting macOS arm64 runners may "
    "experience longer queue times."
)
TIMED_OUT = "The job has exceeded the maximum execution time of 15m0s"
SUPERSEDED = "Canceling since a higher priority waiting request for ci-CI-59 exists"

RUN_ID = 43188617613
HOSTED_RUNNER_ID = 1000000926


def passed(name: str) -> Job:
    return Job(name=name, conclusion="success", runner_id=HOSTED_RUNNER_ID)


def skipped(name: str = "detect version bump") -> Job:
    return Job(name=name, conclusion="skipped", runner_id=None)


def unassigned(name: str = "check (macos-latest)") -> Job:
    return Job(
        name=name,
        conclusion="cancelled",
        runner_id=0,
        annotations=(NOT_ACQUIRED, CAPACITY_NOTICE),
    )


def run(*jobs: Job, conclusion: str = "failure", attempt: int = 1) -> Run:
    return Run(id=RUN_ID, attempt=attempt, conclusion=conclusion, jobs=jobs)


def still_latest(inspected: Run) -> Attempt:
    return Attempt(run_id=inspected.id, number=inspected.attempt)


def decide_while_latest(inspected: Run):
    return decide(inspected, latest=still_latest(inspected))


class RerunUnassignedRunnerTests(unittest.TestCase):
    def test_a_run_that_failed_only_because_a_job_never_got_a_runner_is_rerun(
        self,
    ) -> None:
        decision = decide_while_latest(
            run(passed("check (ubuntu-latest)"), unassigned())
        )

        self.assertTrue(decision.rerun, decision.reason)

    def test_the_reason_names_the_jobs_that_never_got_a_runner(self) -> None:
        decision = decide_while_latest(
            run(unassigned("check (macos-latest)"), unassigned("lint (macos-latest)"))
        )

        self.assertIn("check (macos-latest)", decision.reason)
        self.assertIn("lint (macos-latest)", decision.reason)

    def test_skipped_jobs_do_not_count_as_unassigned_or_as_failures(self) -> None:
        decision = decide_while_latest(
            run(skipped(), passed("check (ubuntu-latest)"), unassigned())
        )

        self.assertTrue(decision.rerun, decision.reason)

    def test_a_job_that_timed_out_on_a_runner_is_not_rerun(self) -> None:
        timed_out = Job(
            name="check (macos-latest)",
            conclusion="cancelled",
            runner_id=HOSTED_RUNNER_ID,
            annotations=(TIMED_OUT, "The operation was canceled.", CAPACITY_NOTICE),
        )

        decision = decide_while_latest(run(timed_out, conclusion="cancelled"))

        self.assertFalse(decision.rerun, decision.reason)

    def test_a_queued_job_cancelled_by_a_newer_push_is_not_rerun(self) -> None:
        superseded = Job(
            name="check (macos-latest)",
            conclusion="cancelled",
            runner_id=0,
            annotations=(SUPERSEDED, CAPACITY_NOTICE),
        )

        decision = decide_while_latest(run(superseded))

        self.assertFalse(decision.rerun, decision.reason)

    def test_a_real_failure_alongside_an_unassigned_job_is_not_rerun(self) -> None:
        failed = Job(
            name="check (ubuntu-latest)",
            conclusion="failure",
            runner_id=HOSTED_RUNNER_ID,
        )

        decision = decide_while_latest(run(failed, unassigned()))

        self.assertFalse(decision.rerun, decision.reason)
        self.assertIn("check (ubuntu-latest)", decision.reason)

    def test_a_job_that_ran_cannot_claim_it_never_got_a_runner(self) -> None:
        forged = Job(
            name="check (ubuntu-latest)",
            conclusion="failure",
            runner_id=HOSTED_RUNNER_ID,
            annotations=(NOT_ACQUIRED,),
        )

        decision = decide_while_latest(run(forged))

        self.assertFalse(decision.rerun, decision.reason)

    def test_a_run_that_did_not_fail_is_not_rerun(self) -> None:
        for conclusion in ("success", "cancelled"):
            with self.subTest(conclusion=conclusion):
                decision = decide_while_latest(run(unassigned(), conclusion=conclusion))

                self.assertFalse(decision.rerun, decision.reason)

    def test_the_last_allowed_attempt_is_not_rerun(self) -> None:
        decision = decide_while_latest(run(unassigned(), attempt=MAX_RUN_ATTEMPTS))

        self.assertFalse(decision.rerun, decision.reason)

    def test_an_attempt_before_the_last_allowed_one_is_rerun(self) -> None:
        decision = decide_while_latest(run(unassigned(), attempt=MAX_RUN_ATTEMPTS - 1))

        self.assertTrue(decision.rerun, decision.reason)

    def test_a_run_superseded_by_a_newer_run_of_its_branch_is_not_rerun(self) -> None:
        decision = decide(
            run(unassigned()), latest=Attempt(run_id=RUN_ID + 1, number=1)
        )

        self.assertFalse(decision.rerun, decision.reason)

    def test_an_attempt_the_run_has_already_moved_past_is_not_rerun(self) -> None:
        decision = decide(
            run(unassigned(), attempt=1), latest=Attempt(run_id=RUN_ID, number=2)
        )

        self.assertFalse(decision.rerun, decision.reason)

    def test_a_run_whose_branch_has_no_listed_runs_is_not_rerun(self) -> None:
        decision = decide(run(unassigned()), latest=None)

        self.assertFalse(decision.rerun, decision.reason)


if __name__ == "__main__":
    unittest.main()
