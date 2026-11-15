from collections import OrderedDict
from ipaddress import ip_network
from json import load
import os
from shlex import quote
from time import monotonic, sleep

from alibabacloud_ecs20140526.client import Client as EcsClient
from alibabacloud_ecs20140526 import models as ecs
from alibabacloud_vpc20160428.client import Client as VpcClient
from alibabacloud_vpc20160428 import models as vpc
from alibabacloud_tea_openapi import models as open_api
from alibabacloud_tea_util.models import RuntimeOptions

from benchmark.utils import BenchError, Print
from alibaba.settings import Settings, SettingsError


class InstanceManager:
    TAG_KEY = 'JcarusTestbed'
    BATCH_SIZE = 100
    WAIT_TIMEOUT = 600
    POLL_INTERVAL = 3
    SYSTEM_DISK_SIZE = 200
    SYSTEM_DISK_CATEGORY = 'cloud_essd'
    PUBLIC_BANDWIDTH = 100
    INGRESS_CIDR = '0.0.0.0/0'

    def __init__(self, settings):
        self.settings = settings
        self.clients = OrderedDict()
        self.vpc_clients = OrderedDict()
        self.networks = {}
        self.runtime = RuntimeOptions(connect_timeout=10000, read_timeout=30000)
        try:
            if settings.accesskey_path is not None:
                with open(settings.accesskey_path) as source:
                    credentials = load(source)
                access_key_id = credentials['AccessKey ID']
                access_key_secret = credentials['AccessKey Secret']
            else:
                access_key_id = os.environ.get('ALIBABA_CLOUD_ACCESS_KEY_ID')
                access_key_secret = os.environ.get('ALIBABA_CLOUD_ACCESS_KEY_SECRET')
            if not all(isinstance(value, str) and value.strip()
                       for value in (access_key_id, access_key_secret)):
                raise ValueError('Supply an AccessKey file or ALIBABA_CLOUD_ACCESS_KEY_ID and ALIBABA_CLOUD_ACCESS_KEY_SECRET')
            for region in settings.regions:
                config = open_api.Config(
                    access_key_id=access_key_id, access_key_secret=access_key_secret,
                    region_id=region, endpoint=f'ecs.{region}.aliyuncs.com',
                )
                self.clients[region] = EcsClient(config)
                self.vpc_clients[region] = VpcClient(open_api.Config(
                    access_key_id=access_key_id, access_key_secret=access_key_secret,
                    region_id=region, endpoint=f'vpc.{region}.aliyuncs.com',
                ))
        except Exception as error:
            raise BenchError('Failed to initialize Alibaba Cloud credentials or clients', error) from error

    @classmethod
    def make(cls, settings_file='settings.json'):
        try:
            return cls(Settings.load(settings_file))
        except SettingsError as error:
            raise BenchError('Failed to load settings', error) from error

    def _call(self, region, operation, request):
        try:
            response = getattr(self.clients[region], operation + '_with_options')(request, self.runtime)
            body = response.to_map()['body']
            for result in body.get('InstanceResponses', {}).get('InstanceResponse', []):
                if str(result.get('Code')) != '200':
                    raise RuntimeError(f"{result.get('InstanceId')}: {result.get('Code')} {result.get('Message')}")
            return body
        except Exception as error:
            raise BenchError(f'Alibaba Cloud {operation} failed in {region}', error) from error

    def _owned(self, resource):
        return any(tag.get('TagKey') == self.TAG_KEY
                   and tag.get('TagValue') == self.settings.instance_name
                   for tag in resource.get('Tags', {}).get('Tag', []))

    def _instances(self, region):
        instances, token = [], None
        while True:
            body = self._call(region, 'describe_instances', ecs.DescribeInstancesRequest(
                region_id=region, max_results=100, next_token=token,
                tag=[ecs.DescribeInstancesRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
            ))
            instances.extend(instance for instance in body['Instances']['Instance'] if self._owned(instance))
            token = body.get('NextToken')
            if not token:
                return sorted(instances, key=lambda instance: instance['InstanceId'])

    def _security_groups(self, region):
        groups, token = [], None
        while True:
            body = self._call(region, 'describe_security_groups', ecs.DescribeSecurityGroupsRequest(
                region_id=region, max_results=100, next_token=token,
                tag=[ecs.DescribeSecurityGroupsRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
            ))
            groups.extend(group for group in body['SecurityGroups']['SecurityGroup'] if self._owned(group))
            token = body.get('NextToken')
            if not token:
                return sorted(groups, key=lambda group: group['SecurityGroupId'])

    def _wait_instances(self, region, instance_ids, states=None, public_ip=False):
        remaining = set(instance_ids)
        deadline = monotonic() + self.WAIT_TIMEOUT
        while remaining:
            instances = {instance['InstanceId']: instance for instance in self._instances(region)}
            if states is None:
                remaining.intersection_update(instances)
            else:
                remaining = {
                    instance_id for instance_id in remaining
                    if instance_id not in instances
                    or instances[instance_id]['Status'] not in states
                    or (public_ip and not instances[instance_id].get('PublicIpAddress', {}).get('IpAddress'))
                }
            if not remaining:
                return
            if monotonic() >= deadline:
                target = 'deleted' if states is None else '/'.join(sorted(states))
                raise BenchError(f'Timed out waiting for ECS instances in {region}',
                                 TimeoutError(f'{sorted(remaining)} did not reach {target}'))
            sleep(self.POLL_INTERVAL)

    def _permissions(self):
        rules = [(22, 'SSH access')]
        rules.extend((getattr(self.settings, name + '_port'), name)
                     for name in ('consensus', 'smvba', 'mempool', 'front'))
        return [
            ecs.AuthorizeSecurityGroupRequestPermissions(
                ip_protocol='tcp', port_range=f'{port}/{port}', priority='1',
                policy='accept', nic_type='intranet', description=description,
                source_cidr_ip=self.INGRESS_CIDR,
            )
            for port, description in rules
        ]

    @staticmethod
    def _permission_key(permission):
        return (permission.get('IpProtocol', '').lower(), permission.get('PortRange'),
                permission.get('SourceCidrIp') or permission.get('Ipv6SourceCidrIp'),
                permission.get('Policy', 'accept'), str(permission.get('Priority', '1')))

    def _vpc_call(self, region, operation, request):
        response = getattr(self.vpc_clients[region], operation + '_with_options')(request, self.runtime)
        return response.to_map()['body']

    def _wait_network(self, region, operation, request, status):
        deadline = monotonic() + self.WAIT_TIMEOUT
        while True:
            body = self._vpc_call(region, operation, request)
            if body['Status'] == status:
                return body
            if monotonic() >= deadline:
                raise TimeoutError(f'{operation} in {region} did not reach {status}: {body["Status"]}')
            sleep(self.POLL_INTERVAL)

    def _available_zones(self, region):
        zones = None
        for resource_type, value in (
                ('InstanceType', self.settings.instance_type),
                ('SystemDisk', self.SYSTEM_DISK_CATEGORY)):
            body = self._call(region, 'describe_available_resource', ecs.DescribeAvailableResourceRequest(
                region_id=region, destination_resource=resource_type, resource_type='instance',
                instance_type=self.settings.instance_type, system_disk_category=self.SYSTEM_DISK_CATEGORY,
                instance_charge_type='PostPaid', io_optimized='optimized', network_category='vpc',
            ))
            available = {
                zone['ZoneId'] for zone in body['AvailableZones']['AvailableZone']
                if zone['Status'] == 'Available'
                and any(
                    resource['Type'] == resource_type
                    and any(item['Value'] == value and item['Status'] == 'Available'
                            for item in resource['SupportedResources']['SupportedResource'])
                    for resource in zone['AvailableResources']['AvailableResource']
                )
            }
            zones = available if zones is None else zones & available
        if not zones:
            raise ValueError(
                f'No available zone for {self.settings.instance_type} with '
                f'{self.SYSTEM_DISK_CATEGORY} in {region}'
            )
        return sorted(zones)

    def _network(self, region, instances):
        if region in self.networks:
            return self.networks[region]
        try:
            zones = self._available_zones(region)
            name = self.settings.instance_name
            matches, page = [], 1
            while True:
                body = self._vpc_call(region, 'describe_vpcs', vpc.DescribeVpcsRequest(
                    region_id=region, vpc_name=name, page_number=page, page_size=50,
                ))
                batch = body['Vpcs']['Vpc']
                matches.extend(item for item in batch if item['VpcName'] == name)
                if not batch or page * 50 >= body['TotalCount']:
                    break
                page += 1
            if len(matches) > 1:
                raise ValueError(
                    f'Expected at most one VPC named {name!r}, found {len(matches)}; '
                    'rename the duplicate VPCs before creating instances'
                )
            if matches:
                vpc_id = matches[0]['VpcId']
            else:
                body = self._vpc_call(region, 'create_vpc', vpc.CreateVpcRequest(
                    region_id=region, vpc_name=name, cidr_block='10.0.0.0/16',
                    description='Jcarus benchmark testbed',
                    tag=[vpc.CreateVpcRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
                ))
                vpc_id = body['VpcId']
                Print.info(f'Created VPC {vpc_id} in {region}')
            attributes = self._wait_network(
                region, 'describe_vpc_attribute',
                vpc.DescribeVpcAttributeRequest(region_id=region, vpc_id=vpc_id), 'Available',
            )
            switches, page = [], 1
            while True:
                body = self._vpc_call(region, 'describe_vswitches', vpc.DescribeVSwitchesRequest(
                    region_id=region, vpc_id=vpc_id, page_number=page, page_size=50,
                ))
                batch = body['VSwitches']['VSwitch']
                switches.extend(batch)
                if not batch or page * 50 >= body['TotalCount']:
                    break
                page += 1
            switch_id = None
            for switch in sorted(switches, key=lambda item: (item['Status'] != 'Available', item['VSwitchId'])):
                if switch['ZoneId'] not in zones or switch['Status'] not in {'Pending', 'Available'}:
                    continue
                if switch['Status'] == 'Pending':
                    switch = self._wait_network(
                        region, 'describe_vswitch_attributes',
                        vpc.DescribeVSwitchAttributesRequest(
                            region_id=region, v_switch_id=switch['VSwitchId'],
                        ), 'Available',
                    )
                if switch['AvailableIpAddressCount'] >= instances:
                    switch_id = switch['VSwitchId']
                    break
            if switch_id is None:
                cidr = ip_network(attributes['CidrBlock'])
                prefix = max(cidr.prefixlen, min(24, 32 - (instances + 3).bit_length()))
                if prefix > 29 or 2 ** (32 - prefix) - 4 < instances:
                    raise ValueError(f'VPC {vpc_id} cannot fit {instances} instances in one vSwitch')
                occupied = [ip_network(switch['CidrBlock']) for switch in switches]
                subnet = next(
                    (candidate for candidate in cidr.subnets(new_prefix=prefix)
                     if not any(candidate.overlaps(used) for used in occupied)), None,
                )
                if subnet is None:
                    raise ValueError(
                        f'No free /{prefix} subnet in VPC {vpc_id}; free subnet space before creating instances'
                    )
                body = self._vpc_call(region, 'create_vswitch', vpc.CreateVSwitchRequest(
                    region_id=region, vpc_id=vpc_id, zone_id=zones[0], cidr_block=str(subnet),
                    v_switch_name=self.settings.instance_name, description='Jcarus benchmark testbed',
                    tag=[vpc.CreateVSwitchRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
                ))
                switch_id = body['VSwitchId']
                Print.info(f'Created vSwitch {switch_id} in {region} ({zones[0]}, {subnet})')
                self._wait_network(
                    region, 'describe_vswitch_attributes',
                    vpc.DescribeVSwitchAttributesRequest(region_id=region, v_switch_id=switch_id),
                    'Available',
                )
            network = {'vpc_id': vpc_id, 'vswitch_id': switch_id}
            self.networks[region] = network
            Print.info(f'Using VPC {vpc_id}, vSwitch {switch_id} in {region}')
            return network
        except Exception as error:
            raise BenchError(f'Failed to prepare VPC and vSwitch in {region}', error) from error

    def _security_group(self, region):
        vpc_id = self.networks[region]['vpc_id']
        groups = [group for group in self._security_groups(region) if group['VpcId'] == vpc_id]
        if groups:
            group_id = groups[0]['SecurityGroupId']
        else:
            body = self._call(region, 'create_security_group', ecs.CreateSecurityGroupRequest(
                region_id=region, vpc_id=vpc_id,
                security_group_name=self.settings.instance_name,
                description='Jcarus benchmark testbed',
                tag=[ecs.CreateSecurityGroupRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
            ))
            group_id = body['SecurityGroupId']
        existing, token = set(), None
        while True:
            body = self._call(region, 'describe_security_group_attribute', ecs.DescribeSecurityGroupAttributeRequest(
                region_id=region, security_group_id=group_id, direction='ingress',
                max_results=100, next_token=token,
            ))
            existing.update(self._permission_key(permission)
                            for permission in body['Permissions']['Permission'])
            token = body.get('NextToken')
            if not token:
                break
        missing = [permission for permission in self._permissions()
                   if self._permission_key(permission.to_map()) not in existing]
        for offset in range(0, len(missing), self.BATCH_SIZE):
            self._call(region, 'authorize_security_group', ecs.AuthorizeSecurityGroupRequest(
                region_id=region, security_group_id=group_id,
                permissions=missing[offset:offset + self.BATCH_SIZE],
            ))
        return group_id

    def _image(self, region):
        images, page = [], 1
        while True:
            body = self._call(region, 'describe_images', ecs.DescribeImagesRequest(
                region_id=region, status='Available', image_owner_alias='system',
                instance_type=self.settings.instance_type, ostype='linux',
                architecture='x86_64', image_family='acs:ubuntu_22_04_x64',
                page_number=page, page_size=100,
            ))
            batch = body['Images']['Image']
            images.extend(batch)
            if not batch or page * 100 >= body['TotalCount']:
                break
            page += 1
        if not images:
            raise BenchError(f'No Ubuntu 22.04 x86_64 system image in {region}',
                             ValueError('Choose a region and instance type supporting an official Ubuntu 22.04 x86_64 image'))
        return max(images, key=lambda image: (image.get('CreationTime', ''), image['ImageId']))['ImageId']

    @staticmethod
    def _positive_count(value, name, allow_zero=False):
        if type(value) is not int or value < (0 if allow_zero else 1):
            raise BenchError(f'Invalid {name}', ValueError('Expected a non-negative integer' if allow_zero else 'Expected a positive integer'))

    def create_instances(self, instances):
        self._positive_count(instances, 'instance count')
        for region in self.clients:
            self._network(region, instances)
        created = OrderedDict()
        for region in self.clients:
            group_id, image_id = self._security_group(region), self._image(region)
            created[region] = []
            for offset in range(0, instances, self.BATCH_SIZE):
                amount = min(self.BATCH_SIZE, instances - offset)
                body = self._call(region, 'run_instances', ecs.RunInstancesRequest(
                    region_id=region, image_id=image_id, instance_type=self.settings.instance_type,
                    instance_name=self.settings.instance_name, instance_charge_type='PostPaid',
                    internet_charge_type='PayByTraffic', internet_max_bandwidth_out=self.PUBLIC_BANDWIDTH,
                    key_pair_name=self.settings.key_name, security_group_id=group_id,
                    v_switch_id=self.networks[region]['vswitch_id'],
                    system_disk=ecs.RunInstancesRequestSystemDisk(
                        category=self.SYSTEM_DISK_CATEGORY, size=str(self.SYSTEM_DISK_SIZE)),
                    amount=amount, min_amount=amount,
                    tag=[ecs.RunInstancesRequestTag(key=self.TAG_KEY, value=self.settings.instance_name)],
                ))
                ids = body['InstanceIdSets']['InstanceIdSet']
                if len(ids) != amount:
                    raise BenchError(f'Incomplete ECS creation in {region}',
                                     RuntimeError(f'Expected {amount} instances, received {len(ids)}: {ids}'))
                created[region].extend(ids)
        Print.info('Waiting for all new ECS instances to be Running with public IPs...')
        for region, ids in created.items():
            self._wait_instances(region, ids, {'Running'}, public_ip=True)
        Print.heading(f'Successfully created {instances * len(self.clients)} new instances')

    def start_instances(self, max):
        self._positive_count(max, 'maximum instance count', allow_zero=True)
        targets = OrderedDict()
        for region in self.clients:
            ids = [instance['InstanceId'] for instance in self._instances(region)
                   if instance['Status'] == 'Stopped'][:max]
            targets[region] = ids
            for offset in range(0, len(ids), self.BATCH_SIZE):
                self._call(region, 'start_instances', ecs.StartInstancesRequest(
                    region_id=region, instance_id=ids[offset:offset + self.BATCH_SIZE],
                    batch_optimization='AllTogether',
                ))
        for region, ids in targets.items():
            self._wait_instances(region, ids, {'Running'}, public_ip=True)
        Print.heading(f'Started {sum(len(ids) for ids in targets.values())} instances')

    def stop_instances(self):
        targets = OrderedDict()
        for region in self.clients:
            instances = self._instances(region)
            targets[region] = [instance['InstanceId'] for instance in instances]
            transitioning = [instance['InstanceId'] for instance in instances
                             if instance['Status'] not in {'Running', 'Stopping', 'Stopped'}]
            self._wait_instances(region, transitioning, {'Running'})
            ids = [instance['InstanceId'] for instance in self._instances(region)
                   if instance['InstanceId'] in targets[region] and instance['Status'] == 'Running']
            for offset in range(0, len(ids), self.BATCH_SIZE):
                self._call(region, 'stop_instances', ecs.StopInstancesRequest(
                    region_id=region, instance_id=ids[offset:offset + self.BATCH_SIZE],
                    batch_optimization='AllTogether', stopped_mode='KeepCharging',
                ))
        for region, ids in targets.items():
            self._wait_instances(region, ids, {'Stopped'})
        Print.heading(f'Stopped {sum(len(ids) for ids in targets.values())} instances')

    def terminate_instances(self):
        targets = OrderedDict()
        for region in self.clients:
            instances = self._instances(region)
            targets[region] = [instance['InstanceId'] for instance in instances]
            transitioning = [instance['InstanceId'] for instance in instances
                             if instance['Status'] not in {'Running', 'Stopped'}]
            self._wait_instances(region, transitioning, {'Running', 'Stopped'})
            ids = targets[region]
            for offset in range(0, len(ids), self.BATCH_SIZE):
                self._call(region, 'delete_instances', ecs.DeleteInstancesRequest(
                    region_id=region, instance_id=ids[offset:offset + self.BATCH_SIZE], force=True,
                ))
        for region, ids in targets.items():
            self._wait_instances(region, ids)
            groups = self._security_groups(region)
            for group in groups:
                self._call(region, 'delete_security_group', ecs.DeleteSecurityGroupRequest(
                    region_id=region, security_group_id=group['SecurityGroupId'],
                ))
            pending = {group['SecurityGroupId'] for group in groups}
            deadline = monotonic() + self.WAIT_TIMEOUT
            while pending:
                pending.intersection_update(group['SecurityGroupId'] for group in self._security_groups(region))
                if not pending:
                    break
                if monotonic() >= deadline:
                    raise BenchError(f'Timed out deleting security groups in {region}', TimeoutError(str(sorted(pending))))
                sleep(self.POLL_INTERVAL)
        Print.heading(f'Testbed of {sum(len(ids) for ids in targets.values())} instances destroyed')

    def hosts(self, flat=False):
        hosts = OrderedDict()
        for region in self.clients:
            hosts[region] = [instance['PublicIpAddress']['IpAddress'][0]
                             for instance in self._instances(region)
                             if instance['Status'] == 'Running'
                             and instance.get('PublicIpAddress', {}).get('IpAddress')]
        return [ip for ips in hosts.values() for ip in ips] if flat else hosts

    def print_info(self):
        hosts = self.hosts()
        lines = []
        for region, ips in hosts.items():
            lines.append(f'\n Region: {region.upper()}')
            for index, ip in enumerate(ips):
                command = f'ssh -i {quote(self.settings.key_path)} {quote(self.settings.ssh_user + "@" + ip)}'
                lines.append(f' {index}\t{command}')
        separator = '-' * 64
        text = '\n'.join(lines)
        print(f'\n{separator}\n INFO:\n{separator}\n'
              f' Available machines: {sum(len(ips) for ips in hosts.values())}\n'
              f'{text}\n{separator}\n')
