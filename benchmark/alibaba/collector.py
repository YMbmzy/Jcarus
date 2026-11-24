from copy import copy
from dataclasses import dataclass
from ipaddress import ip_address, ip_network
from json import load
from pathlib import Path
from shlex import join as shell_join
import re
import subprocess

from alibabacloud_ecs20140526 import models as ecs
from fabric import Connection
from paramiko import RejectPolicy

from alibaba.collector_results import download_results
from alibaba.install import setup_collector
from alibaba.instance import InstanceManager
from alibaba.settings import Settings, SettingsError
from benchmark.utils import Print


@dataclass(frozen=True)
class CollectorConfig:
    name: str = 'JcarusCollector'
    region: str = 'cn-hongkong'
    instance_type: str = 'ecs.g9i.xlarge'
    ssh_cidr: str = '0.0.0.0/0'

    @classmethod
    def load(cls, settings_file, experiment_regions):
        with Path(settings_file).open() as source:
            values = load(source).get('collector', {})
        if not isinstance(values, dict):
            raise SettingsError('collector must be a JSON object')
        config = cls(
            name=values.get('name', cls.name),
            region=values.get('region', cls.region),
            instance_type=values.get('type', cls.instance_type),
            ssh_cidr=values.get('ssh_cidr', cls.ssh_cidr),
        )
        if not isinstance(config.name, str) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{1,63}', config.name):
            raise SettingsError('collector.name must be a valid 2-64 character instance name')
        if not isinstance(config.region, str) or not re.fullmatch(r'[a-z]+(?:-[a-z0-9]+)+', config.region):
            raise SettingsError('collector.region must be an Alibaba Cloud region ID')
        if config.region in experiment_regions:
            raise SettingsError('collector.region must differ from every experiment region')
        if not isinstance(config.instance_type, str) or not config.instance_type.startswith('ecs.'):
            raise SettingsError('collector.type must be an ECS instance type')
        try:
            network = ip_network(config.ssh_cidr)
        except (ValueError, TypeError) as error:
            raise SettingsError('collector.ssh_cidr must be an IPv4 CIDR') from error
        if network.version != 4:
            raise SettingsError('collector.ssh_cidr must be an IPv4 CIDR')
        return config


class CollectorManager(InstanceManager):
    TAG_KEY = 'JcarusCollector'

    def __init__(self, settings, ssh_cidr):
        self.ssh_cidr = ssh_cidr
        super().__init__(settings)

    def _owned(self, resource):
        # Never adopt a consensus resource, even if it was accidentally double-tagged.
        tags = resource.get('Tags', {}).get('Tag', [])
        return super()._owned(resource) and not any(
            tag.get('TagKey') == InstanceManager.TAG_KEY for tag in tags
        )

    def _instances(self, region):
        instances = super()._instances(region)
        if len(instances) > 1:
            raise RuntimeError('Multiple collector instances match; resolve duplicate tags in the console first')
        return instances

    def _permissions(self):
        return [ecs.AuthorizeSecurityGroupRequestPermissions(
            ip_protocol='tcp', port_range='22/22', priority='1', policy='accept',
            nic_type='intranet', description='Jcarus collector SSH',
            source_cidr_ip=self.ssh_cidr,
        )]


class Collector:
    def __init__(self, ctx, settings_file='settings.json'):
        self.ctx = ctx
        self.settings_file = Path(settings_file).expanduser().resolve()
        self.settings = Settings.load(str(self.settings_file))
        self.config = CollectorConfig.load(self.settings_file, self.settings.regions)
        scoped = copy(self.settings)
        scoped.instance_name = self.config.name
        scoped.instance_type = self.config.instance_type
        scoped.regions = [self.config.region]
        self.manager = CollectorManager(scoped, self.config.ssh_cidr)

    def _instance(self, required=True, running=False):
        instances = self.manager._instances(self.config.region)
        if not instances:
            if required:
                raise RuntimeError('No collector exists; run fab collector-create first')
            return None
        instance = instances[0]
        if running and instance['Status'] != 'Running':
            raise RuntimeError('Collector is not Running; fab collector-create can start an existing stopped collector')
        return instance

    @staticmethod
    def _ip(instance):
        addresses = instance.get('PublicIpAddress', {}).get('IpAddress', [])
        if not addresses:
            raise RuntimeError('Collector has no public IP address')
        address = str(ip_address(addresses[0]))
        if ':' in address:
            raise RuntimeError('Collector requires a public IPv4 address')
        return address

    def _ssh_args(self):
        return [
            'ssh', '-i', self.settings.key_path,
            '-o', 'IdentitiesOnly=yes', '-o', 'ForwardAgent=no',
            '-o', 'StrictHostKeyChecking=ask', '-o', 'ConnectTimeout=15',
        ]

    def _target(self, instance):
        return f'{self.settings.ssh_user}@{self._ip(instance)}'

    def _remote_home(self, instance):
        result = subprocess.run(
            self._ssh_args() + [self._target(instance), 'printf \'%s\' "$HOME"'],
            check=True, text=True, stdout=subprocess.PIPE,
        )
        home = result.stdout
        if not home.startswith('/') or '\n' in home or '\x00' in home:
            raise RuntimeError('SSH did not return an absolute remote home directory')
        return home

    def _ensure_key_pair(self):
        if not Path(self.settings.key_path).is_file():
            raise FileNotFoundError(self.settings.key_path)
        # Derive from the private key, not a possibly stale sibling .pub file.
        public_key = subprocess.run(
            ['ssh-keygen', '-y', '-f', self.settings.key_path],
            check=True, text=True, stdout=subprocess.PIPE,
        ).stdout.strip()
        body = self.manager._call(self.config.region, 'describe_key_pairs', ecs.DescribeKeyPairsRequest(
            region_id=self.config.region, key_pair_name=self.settings.key_name,
            include_public_key=True,
        ))
        pairs = body.get('KeyPairs', {}).get('KeyPair', [])
        if pairs:
            if len(pairs) != 1 or pairs[0].get('PublicKey', '').split()[:2] != public_key.split()[:2]:
                raise RuntimeError('Collector-region key.name already exists with a different or unverifiable public key')
        else:
            self.manager._call(self.config.region, 'import_key_pair', ecs.ImportKeyPairRequest(
                region_id=self.config.region, key_pair_name=self.settings.key_name,
                public_key_body=public_key,
            ))

    def create(self):
        instance = self._instance(required=False)
        if instance is None:
            self._ensure_key_pair()
            Print.warn('Creating one paid collector: Ubuntu 22.04, 200 GiB ESSD, 100 Mbit/s public egress')
            self.manager.create_instances(1)
        else:
            if instance['Status'] not in {'Running', 'Stopped'}:
                self.manager._wait_instances(self.config.region, [instance['InstanceId']], {'Running', 'Stopped'})
                instance = self._instance()
            if instance['Status'] == 'Stopped':
                Print.warn('Starting the existing collector; billing continues')
                self.manager.start_instances(1)
            else:
                Print.info('Reusing the existing collector; no new instance created')
        self.info()

    def setup(self):
        for path in (self.settings.key_path, self.settings.accesskey_path):
            if path is None or not Path(path).is_file():
                raise FileNotFoundError('collector-setup requires readable key.path and key.accesskey files')
        instance = self._instance(running=True)
        home = self._remote_home(instance)
        kwargs = dict(self.ctx.connect_kwargs)
        kwargs['key_filename'] = self.settings.key_path
        with Connection(
            self._ip(instance), user=self.settings.ssh_user, config=self.ctx.config,
            connect_kwargs=kwargs, forward_agent=False,
        ) as connection:
            connection.client.load_system_host_keys()
            connection.client.set_missing_host_key_policy(RejectPolicy())
            setup_collector(connection, self.settings_file, self.settings, home + '/Jcarus')
        Print.info('Collector ready; use fab collector-ssh, then run experiments inside tmux')

    def ssh(self):
        instance = self._instance(running=True)
        command = (
            'cd "$HOME/Jcarus/benchmark" && '
            '. ../.venv/bin/activate && . "$HOME/.cargo/env" && exec bash -i'
        )
        subprocess.run(self._ssh_args() + ['-t', self._target(instance), command], check=True)

    def _destination(self, directory, snapshot):
        destination = (
            Path(directory).expanduser().resolve() if directory else
            self.settings_file.parent / 'collector-results' / ('downloads' if snapshot else 'current')
        )
        protected = [self.settings_file.parent / 'results']
        protected.extend(
            Path(path).resolve().parent for path in (self.settings.key_path, self.settings.accesskey_path) if path
        )
        for path in protected:
            path = path.resolve()
            if destination == path or destination in path.parents or path in destination.parents:
                raise ValueError(f'Download destination overlaps a protected results/credential directory: {path}')
        return destination

    def download(self, directory=None, snapshot=True):
        destination = self._destination(directory, snapshot)
        instance = self._instance(running=True)
        home = self._remote_home(instance)
        source = f'{self._target(instance)}:{home}/Jcarus/benchmark/results/'
        result = download_results(source, self._ssh_args(), destination, snapshot=snapshot)
        Print.info(f'Results downloaded to {result}')
        return result

    def info(self):
        instance = self._instance(required=False)
        lines = [
            f'Collector: {self.config.name}', f'Region: {self.config.region}',
            f'Type: {instance.get("InstanceType", self.config.instance_type) if instance else self.config.instance_type}',
            f'Tag: {CollectorManager.TAG_KEY}={self.config.name}',
            f'Instance: {instance["InstanceId"] if instance else "not created"}',
            f'Status: {instance["Status"] if instance else "absent"}',
            'Remote project: ~/Jcarus', 'Remote results: ~/Jcarus/benchmark/results/',
            f'Local downloads: {self.settings_file.parent / "collector-results" / "downloads"}',
            f'Local incremental: {self.settings_file.parent / "collector-results" / "current"}',
        ]
        if instance and instance.get('PublicIpAddress', {}).get('IpAddress'):
            lines.append(f'Public IP: {self._ip(instance)}')
            lines.append('SSH: ' + shell_join(self._ssh_args() + [self._target(instance)]))
        Print.info('\n'.join(lines))

    def destroy(self):
        instance = self._instance(required=False)
        groups = self.manager._security_groups(self.config.region)
        if instance is None and not groups:
            Print.info('No collector instance or collector security groups to destroy')
            return
        identity = instance['InstanceId'] if instance else self.config.name
        Print.warn(f'Destroying collector {identity} in {self.config.region}; consensus nodes are excluded')
        Print.warn('Remote logs, results and copied credentials may be lost. Download results first. Local downloads are kept.')
        Print.info('Collector security groups: ' + ', '.join(group['SecurityGroupId'] for group in groups))
        if input(f'Type {identity} to confirm destruction: ').strip() != identity:
            Print.info('Collector destruction cancelled')
            return
        self.manager.terminate_instances()
        Print.info('Collector destroyed. VPCs/vSwitches and independently retained disks/snapshots are not deleted; inspect billing in the console.')
