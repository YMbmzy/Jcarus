"""Download collector results without modifying benchmark runs or credentials."""

from datetime import datetime, timezone
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile


def _overlaps(first: Path, second: Path) -> bool:
    return first == second or first in second.parents or second in first.parents


def _validate_destination(directory: Path, ssh_command: list[str]) -> Path:
    destination = directory.expanduser().resolve()
    protected = [
        Path(__file__).resolve().parents[1] / 'results',
        Path.home() / '.ssh',
        Path.home() / '.aliyun',
    ]
    # The parent supplies the configured identity through -i. Protect its source
    # directory as well as the conventional credential locations above.
    for index, argument in enumerate(ssh_command):
        if argument == '-i':
            if index + 1 == len(ssh_command):
                raise ValueError('SSH -i requires an identity file')
            protected.append(Path(ssh_command[index + 1]).expanduser().resolve().parent)
        elif argument.startswith('-i') and len(argument) > 2:
            protected.append(Path(argument[2:]).expanduser().resolve().parent)
    for path in protected:
        if _overlaps(destination, path.resolve()):
            raise ValueError(f'Download destination overlaps protected directory: {path}')
    return destination


def download_results(source: str, ssh_command: list[str], directory: Path,
                     snapshot: bool = False) -> Path:
    """Pull a results directory using rsync and return its absolute destination.

    ``source`` is normally ``user@host:/absolute/results/``; local directory
    sources are also supported. The remote Jcarus helper selects the sender's
    directory without relying on version-dependent rsync argument quoting.

    Incremental downloads compare checksums and preserve local-only files.
    Snapshots use a fresh timestamped directory, removed if the transfer fails.
    No default download location is selected here.
    """
    if not source or not source.endswith('/'):
        raise ValueError('Results source must be a directory with a trailing slash')
    if not ssh_command or not all(isinstance(part, str) and part for part in ssh_command):
        raise ValueError('SSH command must contain non-empty argv strings')
    root = _validate_destination(directory, ssh_command)
    root.mkdir(parents=True, exist_ok=True)
    if snapshot:
        timestamp = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H-%M-%SZ-')
        destination = Path(tempfile.mkdtemp(prefix=timestamp, dir=root))
    else:
        destination = root
    command = ['rsync', '--archive', '--checksum', '-e', shlex.join(ssh_command)]
    host, separator, remote_path = source.partition(':')
    if separator and '/' not in host:
        # Some macOS rsync versions parse --rsync-path before invoking SSH.
        # Hex keeps the directory one shell-safe token through both parsers.
        server = 'python3 Jcarus/benchmark/alibaba/collector_results.py --sender-root '
        command.extend(['--rsync-path', server + remote_path.encode().hex()])
        source = host + ':./'
    command.extend(['--', source, str(destination) + '/'])
    try:
        subprocess.run(command, check=True)
    except BaseException as error:
        if snapshot:
            try:
                shutil.rmtree(destination)
            except OSError as cleanup_error:
                raise RuntimeError(
                    f'Download to {destination} failed; partial snapshot could not '
                    f'be removed: {cleanup_error}'
                ) from error
        if isinstance(error, (OSError, subprocess.CalledProcessError)):
            raise RuntimeError(f'Download to {destination} failed: {error}') from error
        raise
    return destination


if __name__ == '__main__':
    if len(sys.argv) < 4 or sys.argv[1] != '--sender-root':
        raise SystemExit('This entry point is used by collector result downloads')
    os.chdir(bytes.fromhex(sys.argv[2]).decode())
    os.execvp('rsync', ['rsync', *sys.argv[3:]])
