from json import JSONDecodeError, load
from pathlib import Path
import re


class SettingsError(Exception):
    pass


class Settings:
    def __init__(self, key_name, key_path, ssh_user, consensus_port, smvba_port,
                 mempool_port, front_port, repo_name, repo_url, branch,
                 instance_name, instance_type, regions, accesskey_path=None):
        strings = {
            'key.name': key_name, 'key.path': key_path, 'ssh.user': ssh_user,
            'repo.name': repo_name, 'repo.url': repo_url, 'repo.branch': branch,
            'instances.name': instance_name, 'instances.type': instance_type,
        }
        if accesskey_path is not None:
            strings['key.accesskey'] = accesskey_path
        for name, value in strings.items():
            if not isinstance(value, str) or not value.strip():
                raise SettingsError(f'{name} must be a non-empty string')
        if not re.fullmatch(r'[a-z_][a-z0-9_-]*', ssh_user):
            raise SettingsError('ssh.user must be a Unix login name')
        if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_.-]{1,63}', instance_name):
            raise SettingsError('instances.name must be 2-64 letters, digits, dots, underscores or hyphens, starting with a letter')
        ports = [consensus_port, smvba_port, mempool_port, front_port]
        if any(type(port) is not int or not 1 <= port <= 65535 for port in ports):
            raise SettingsError('Protocol ports must be integers between 1 and 65535')
        if len(set(ports)) != len(ports) or 22 in ports:
            raise SettingsError('Protocol ports must be unique and different from SSH port 22')
        if not isinstance(regions, list) or not regions:
            raise SettingsError('instances.regions must be a non-empty list of region IDs')
        for region in regions:
            if not isinstance(region, str) or not re.fullmatch(r'[a-z]+(?:-[a-z0-9]+)+', region):
                raise SettingsError('Each region must be an Alibaba Cloud region ID')
        if len(set(regions)) != len(regions):
            raise SettingsError('instances.regions must not contain duplicate region IDs')
        self.key_name = key_name
        self.key_path = key_path
        self.accesskey_path = accesskey_path
        self.ssh_user = ssh_user
        self.consensus_port = consensus_port
        self.smvba_port = smvba_port
        self.mempool_port = mempool_port
        self.front_port = front_port
        self.repo_name = repo_name
        self.repo_url = repo_url
        self.branch = branch
        self.instance_name = instance_name
        self.instance_type = instance_type
        self.regions = list(regions)

    @classmethod
    def load(cls, settings_file='settings.json'):
        try:
            filename = Path(settings_file).expanduser().resolve()
            with filename.open() as source:
                data = load(source)
            key, ssh, ports = data['key'], data['ssh'], data['ports']
            repo, instances = data['repo'], data['instances']
            settings = cls(
                key_name=key['name'], key_path=key['path'], ssh_user=ssh['user'],
                consensus_port=ports['consensus'], smvba_port=ports['smvba'],
                mempool_port=ports['mempool'], front_port=ports['front'],
                repo_name=repo['name'], repo_url=repo['url'], branch=repo['branch'],
                instance_name=instances['name'], instance_type=instances['type'],
                regions=instances['regions'],
                accesskey_path=key.get('accesskey'),
            )
            for attribute in ('key_path', 'accesskey_path'):
                value = getattr(settings, attribute)
                if value is not None:
                    path = Path(value).expanduser()
                    if not path.is_absolute():
                        path = filename.parent / path
                    setattr(settings, attribute, str(path.resolve()))
            return settings
        except SettingsError:
            raise
        except (OSError, JSONDecodeError, KeyError, TypeError, ValueError) as error:
            raise SettingsError(f'Invalid settings: {error}') from error
