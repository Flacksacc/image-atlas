"""Cooperative cancellation for foreground mining commands."""
import json
import os
import tempfile
import uuid
from pathlib import Path

import psutil


class MiningCancelled(BaseException):
    pass


def job_directory() -> Path:
    return Path(os.environ.get("IMAGE_CLI_JOB_DIR", Path(tempfile.gettempdir()) / "image-cli-jobs"))


def active_jobs() -> list[dict]:
    jobs = []
    directory = job_directory()
    if not directory.exists():
        return jobs
    for path in directory.glob("*.json"):
        try:
            job = json.loads(path.read_text(encoding="utf-8"))
            process = psutil.Process(job["pid"])
            if abs(process.create_time() - job["created_at"]) < 2 and process.is_running():
                job["cancel_requested"] = path.with_suffix(".cancel").exists()
                jobs.append(job)
            else:
                path.unlink(missing_ok=True)
                path.with_suffix(".cancel").unlink(missing_ok=True)
        except (OSError, ValueError, KeyError, psutil.Error):
            continue
    return jobs


class MineJob:
    def __init__(self, source: str):
        from .catalogs import resolve
        self.catalog = resolve()
        self.id = uuid.uuid4().hex[:12]
        self.directory = job_directory()
        self.directory.mkdir(parents=True, exist_ok=True)
        self.record = self.directory / f"{self.id}.json"
        self.cancel_file = self.record.with_suffix(".cancel")
        from .catalogs import registry_lock
        with registry_lock():
            self.record.write_text(json.dumps({"id": self.id, "pid": os.getpid(),
                                               "created_at": psutil.Process().create_time(),
                                               "source": source, "catalog_id": self.catalog.id}), encoding="utf-8")

    def update(self, **metrics):
        data = json.loads(self.record.read_text(encoding='utf-8'))
        data.update(metrics)
        temporary = self.record.with_suffix('.tmp')
        temporary.write_text(json.dumps(data), encoding='utf-8')
        os.replace(temporary, self.record)

    def check(self):
        if self.cancel_file.exists():
            raise MiningCancelled()

    def close(self):
        self.record.unlink(missing_ok=True)
        self.cancel_file.unlink(missing_ok=True)
        self.record.with_suffix('.tmp').unlink(missing_ok=True)


def cancel_job(job_id: str | None) -> str:
    from .catalogs import resolve
    catalog = resolve()
    matches = [job for job in active_jobs() if (job_id is None or job["id"] == job_id)
               and job.get('catalog_id') == catalog.id]
    if len(matches) != 1:
        raise ValueError("Specify a running job ID" if matches else "No matching running job")
    identifier = matches[0]["id"]
    (job_directory() / f"{identifier}.cancel").touch()
    return identifier
