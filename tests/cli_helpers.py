"""Automation tests opt in to the CLI's structured output contract."""
import shlex
from typer.testing import CliRunner


class JsonCliRunner(CliRunner):
    def invoke(self, app, args=None, **kwargs):
        if isinstance(args, str):
            args = shlex.split(args)
        return super().invoke(app, ['--json', *(args or [])], **kwargs)
