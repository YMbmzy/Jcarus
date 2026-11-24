"""Shared node installation and deployment of the independent collector."""

from copy import copy
from io import StringIO
import json
from os.path import join
from pathlib import Path, PurePosixPath
from shlex import quote

from alibaba.settings import Settings


def installation_command(settings) -> str:
    """Return the same installation command for testbed nodes and collectors."""
    repo = quote(join('.', settings.repo_name))
    branch = quote(settings.branch)
    url = quote(settings.repo_url)
    return ' && '.join([
        'if [ "$(id -u)" -eq 0 ]; then SUDO=; else SUDO=sudo; fi',
        'if command -v cloud-init >/dev/null 2>&1; then $SUDO cloud-init status --wait; fi',
        '$SUDO env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 update',
        '$SUDO env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 '
        'install -y tmux git curl ca-certificates build-essential cmake clang pkg-config libssl-dev',
        # Install rust (non-interactive) for the configured SSH user.
        'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y',
        '. "$HOME/.cargo/env"',
        'rustup default stable',
        # Clone or update the configured branch in the configured directory.
        f'if [ -d {repo}/.git ]; then '
        f'git -C {repo} fetch -f origin && '
        f'git -C {repo} checkout -f {branch} && '
        f'git -C {repo} pull --ff-only origin {branch}; '
        f'else git clone --branch {branch} -- {url} {repo}; fi',
    ])


def _put_private(connection, source, destination):
    """Upload into a mode-600 temporary file in an already private directory."""
    temporary = connection.run(
        f'umask 077 && mktemp {quote(str(PurePosixPath(destination).parent / ".upload.XXXXXXXXXX"))}',
        hide=True,
    ).stdout.strip()
    try:
        connection.put(source, remote=temporary, preserve_mode=False)
        connection.run(
            f'chmod 600 {quote(temporary)} && mv -f -- {quote(temporary)} {quote(destination)}',
            hide=True,
        )
    finally:
        connection.run(f'rm -f -- {quote(temporary)}', hide=True, warn=True)


# This runs only on the collector, never on the machine deploying it. Keep API
# error text and SDK tracebacks out of the terminal: they may contain credentials.
_VALIDATION = '''\
import subprocess
from alibaba.instance import InstanceManager
from alibaba.settings import Settings

settings = Settings.load('settings.json')
try:
    manager = InstanceManager(settings)
except Exception:
    raise SystemExit('Collector credential validation failed: check the uploaded AccessKey JSON and its permissions.') from None

nodes = []
for region in settings.regions:
    try:
        instances = manager._instances(region)
    except Exception:
        raise SystemExit(f'DescribeInstances validation failed in {region}: check AccessKey validity, ecs:DescribeInstances permission, and API connectivity.') from None
    print(f'DescribeInstances verified in {region}.', flush=True)
    for instance in instances:
        if instance.get('Status') != 'Running':
            continue
        addresses = instance.get('PublicIpAddress', {}).get('IpAddress', [])
        if not addresses:
            raise SystemExit(f'SSH validation unavailable for running node {instance["InstanceId"]} in {region}: no public IP address.')
        nodes.append((region, addresses[0]))

if not nodes:
    print('No running benchmark nodes discovered; SSH verification was NOT performed.', flush=True)
for region, host in nodes:
    command = [
        'ssh', '-i', settings.key_path,
        '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes',
        '-o', 'ForwardAgent=no', '-o', 'StrictHostKeyChecking=accept-new',
        '-o', 'ConnectTimeout=15', '-o', 'ConnectionAttempts=1',
        f'{settings.ssh_user}@{host}', 'true',
    ]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        raise SystemExit(f'SSH validation failed for {host} in {region}: check SSH availability and network/security-group access.') from None
    if result.returncode:
        raise SystemExit(f'SSH validation failed for {host} in {region}: check the copied private key, authorized keys, host identity, and network/security-group access.')
    print(f'SSH verified for {host} in {region}.', flush=True)
'''


def setup_collector(connection, settings_file: Path, settings: Settings, project_dir: str) -> None:
    """Deploy current local benchmark scripts and private credentials, then validate.

    The caller must establish and verify the collector's SSH identity before
    passing its Fabric connection. No cloud calls or node SSH run locally.
    """
    settings_file = Path(settings_file).expanduser().resolve()
    local_root = settings_file.parent
    credentials = []
    for attribute, label in (('key_path', 'SSH private key'), ('accesskey_path', 'Alibaba AccessKey file')):
        value = getattr(settings, attribute, None)
        if not value:
            raise FileNotFoundError(f'Collector setup requires a configured {label}')
        source = Path(value).expanduser()
        if not source.is_file():
            raise FileNotFoundError(f'Collector setup cannot find {label}: {source}')
        # Open only to check readability; never read or print credential contents.
        with source.open('rb'):
            pass
        credentials.append(source)

    project = PurePosixPath(project_dir)
    if not project.is_absolute() or project.name != 'Jcarus' or '..' in project.parts:
        raise ValueError('Collector project directory must be an absolute HOME/Jcarus path')
    home = project.parent
    remote_benchmark = project / 'benchmark'
    ssh_directory = home / '.ssh'
    key_directory = ssh_directory / 'Aliyun'
    access_directory = home / '.aliyun'
    remote_key = str(key_directory / 'id_ed25519')
    remote_access = str(access_directory / 'access.json')

    with settings_file.open() as source:
        uploaded_settings = json.load(source)
    uploaded_settings['key']['path'] = remote_key
    uploaded_settings['key']['accesskey'] = remote_access
    uploaded_settings['repo']['name'] = 'Jcarus'

    sources = {local_root / 'fabfile.py', local_root / 'requirements.txt'}
    sources.update(local_root.glob('*.py'))
    for directory in ('benchmark', 'alibaba'):
        package = local_root / directory
        if not package.is_dir():
            raise FileNotFoundError(f'Collector source directory is missing: {package}')
        sources.update(path for path in package.rglob('*.py') if '__pycache__' not in path.parts)
    for source in sources:
        if not source.is_file():
            raise FileNotFoundError(f'Collector source file is missing: {source}')

    install_settings = copy(settings)
    install_settings.repo_name = str(project)
    connection.run(installation_command(install_settings), hide=False)
    connection.run(' && '.join([
        'if [ "$(id -u)" -eq 0 ]; then SUDO=; else SUDO=sudo; fi',
        '$SUDO env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 '
        'install -y python3.10 python3.10-venv python3.10-dev rsync openssh-client',
        'umask 077',
        'mkdir -p -- ' + ' '.join(quote(str(path)) for path in (ssh_directory, key_directory, access_directory)),
        'chmod 700 ' + ' '.join(quote(str(path)) for path in (ssh_directory, key_directory, access_directory)),
    ]), hide=False)

    # Whitelist Python sources and requirements, not logs, results or generated keys.
    remote_directories = sorted({str((remote_benchmark / source.relative_to(local_root).as_posix()).parent)
                                 for source in sources})
    connection.run('mkdir -p -- ' + ' '.join(map(quote, remote_directories)), hide=True)
    for source in sorted(sources):
        destination = remote_benchmark / source.relative_to(local_root).as_posix()
        connection.put(str(source), remote=str(destination))

    _put_private(connection, str(credentials[0]), remote_key)
    _put_private(connection, str(credentials[1]), remote_access)
    _put_private(connection, StringIO(json.dumps(uploaded_settings, indent=2) + '\n'),
                 str(remote_benchmark / 'settings.json'))

    python = quote(str(project / '.venv' / 'bin' / 'python'))
    connection.run(' && '.join([
        f'python3.10 -m venv {quote(str(project / ".venv"))}',
        f'{python} -m pip install -r {quote(str(remote_benchmark / "requirements.txt"))}',
    ]), hide=False)
    connection.run(
        f'cd {quote(str(remote_benchmark))} && {python} -c {quote(_VALIDATION)}',
        hide=False,
    )
