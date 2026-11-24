from concurrent.futures import ThreadPoolExecutor
from fabric import Connection, GroupResult, ThreadingGroup as Group
from fabric.exceptions import GroupException
from hashlib import sha256
from os.path import basename, splitext
from pathlib import Path
import platform
from time import sleep
from math import ceil
from itertools import zip_longest
from os.path import isfile, join
from shlex import quote
from datetime import datetime
import subprocess

from benchmark.config import Committee, Key, TSSKey, NodeParameters, BenchParameters, ConfigError
from benchmark.utils import BenchError, Print, PathMaker, progress_bar
from benchmark.commands import CommandMaker
from benchmark.logs import LogParser, ParseError
from alibaba.instance import InstanceManager
from alibaba.install import node_installation_command

class FabricError(Exception):
    ''' Wrapper for Fabric exception with a meaningfull error message. '''

    def __init__(self, error):
        assert isinstance(error, GroupException)
        message = list(error.result.values())[-1]
        super().__init__(message)


class ExecutionError(Exception):
    pass


class Bench:
    def __init__(self, ctx):
        self.manager = InstanceManager.make()
        self.settings = self.manager.settings
        if not isfile(self.settings.key_path):
            raise BenchError('Failed to load SSH key', FileNotFoundError(self.settings.key_path))
        self.connect = dict(ctx.connect_kwargs)
        self.connect['key_filename'] = self.settings.key_path

    def _check_stderr(self, output):
        if isinstance(output, dict):
            for x in output.values():
                if x.stderr:
                    raise ExecutionError(x.stderr)
        else:
            if output.stderr:
                raise ExecutionError(output.stderr)

    def install(self):
        Print.info('Preparing nodes (cloud-init, tmux)...')
        hosts = self.manager.hosts(flat=True)
        try:
            g = Group(*hosts, user=self.settings.ssh_user, connect_kwargs=self.connect)
            g.run(node_installation_command(), hide=False)
            Print.heading(f'Initialized testbed of {len(hosts)} nodes')
        except (GroupException, ExecutionError) as e:
            e = FabricError(e) if isinstance(e, GroupException) else e
            raise BenchError('Failed to install repo on testbed', e)

    def kill(self, hosts=[], delete_logs=False):
        assert isinstance(hosts, list)
        assert isinstance(delete_logs, bool)
        hosts = hosts if hosts else self.manager.hosts(flat=True)
        # delete_logs = CommandMaker.clean_logs() if delete_logs else 'true'
        cmd = f'({CommandMaker.kill()} || true)'
        try:
            g = Group(*hosts, user=self.settings.ssh_user, connect_kwargs=self.connect)
            g.run(cmd, hide=True)
        except GroupException as e:
            raise BenchError('Failed to kill nodes', FabricError(e))

    def _select_hosts(self, bench_parameters):
        nodes = max(bench_parameters.nodes)

        # Ensure there are enough hosts.
        hosts = self.manager.hosts()
        if sum(len(x) for x in hosts.values()) < nodes:
            return []

        # Select the hosts in different data centers.
        ordered = [
            host for row in zip_longest(*hosts.values())
            for host in row if host is not None
        ]
        return ordered[:nodes]

    def _parallel(self, hosts, task, prefix=None):
        ''' Run task(index, connection) on all hosts concurrently, like Group. '''
        connections = [
            Connection(host, user=self.settings.ssh_user, connect_kwargs=self.connect)
            for host in hosts
        ]

        def work(index, c):
            try:
                return task(index, c)
            finally:
                c.close()

        with ThreadPoolExecutor(max_workers=max(len(hosts), 1)) as pool:
            futures = [pool.submit(work, i, c) for i, c in enumerate(connections)]
            for future in progress_bar(futures, prefix=prefix) if prefix else futures:
                future.exception()  # Wait for completion.

        # Like ThreadingGroup, list failures last so FabricError reports one.
        results, failures = GroupResult(), {}
        for host, future in zip(hosts, futures):
            if future.exception() is None:
                results[host] = future.result()
            else:
                failures[host] = future.exception()
        if failures:
            results.update(failures)
            raise GroupException(results)
        return results

    def _background_run(self, c, command, log_file):
        name = splitext(basename(log_file))[0]
        pipeline = f'{command} 2>&1 | tee {quote(log_file)}'
        shell_command = f'sh -c {quote(pipeline)}'
        cmd = f'tmux new -d -s {quote(name)} {quote(shell_command)}'
        output = c.run(cmd, hide=True)
        self._check_stderr(output)

    def _update(self, hosts):
        if (platform.system(), platform.machine()) != ('Linux', 'x86_64'):
            raise BenchError(
                'fab remote must run on the collector',
                OSError('Testbed nodes run Linux x86_64 binaries built by the controller'),
            )
        Print.info(f'Building branch "{self.settings.branch}" on the controller...')
        # Never force: collector-setup leaves uploaded scripts/settings as local
        # changes, so stop on conflicts instead of discarding them.
        for cmd in (['fetch', '--quiet', 'origin'],
                    ['checkout', '--quiet', self.settings.branch],
                    ['pull', '--quiet', '--ff-only', 'origin', self.settings.branch]):
            subprocess.run(['git', '-C', '..', *cmd], check=True)
        subprocess.run(CommandMaker.compile().split(), check=True, cwd=PathMaker.node_crate_path())
        cmd = CommandMaker.alias_binaries(PathMaker.binary_path())
        subprocess.run([cmd], shell=True)

        # Upload the same binaries to every node, skipping unchanged ones.
        binaries = ['node', 'client']
        expected = ''.join(
            f'{sha256(Path(PathMaker.binary_path(), name).read_bytes()).hexdigest()}  {name}\n'
            for name in binaries
        )

        def upload(i, c):
            if c.run(f'sha256sum {" ".join(binaries)}', hide=True, warn=True).stdout == expected:
                return
            for name in binaries:
                c.put(join(PathMaker.binary_path(), name), f'.{name}.upload')
            # Rename over old files/symlinks: works even if a binary is still running.
            c.run(' && '.join(f'mv -f .{name}.upload {name}' for name in binaries), hide=True)
        self._parallel(hosts, upload, prefix=f'Uploading binaries to {len(hosts)} nodes:')

    def _config(self, hosts, node_parameters):
        Print.info('Generating configuration files...')

        # Cleanup all local configuration files.
        cmd = CommandMaker.cleanup()
        subprocess.run([cmd], shell=True, stderr=subprocess.DEVNULL)

        # Generate configuration files.
        keys = []
        key_files = [PathMaker.key_file(i) for i in range(len(hosts))]
        for filename in key_files:
            cmd = CommandMaker.generate_key(filename).split()
            subprocess.run(cmd, check=True)
            keys += [Key.from_file(filename)]

        # Generate threshold signature files.
        nodes = len(hosts)
        cmd = './node threshold_keys'
        for i in range(nodes):
            cmd += ' --filename ' + PathMaker.threshold_key_file(i)
        cmd = cmd.split()
        subprocess.run(cmd, capture_output=True, check=True)

        names = [x.name for x in keys]
        smvba_addr = [f'{x}:{self.settings.smvba_port}' for x in hosts]
        consensus_addr = [f'{x}:{self.settings.consensus_port}' for x in hosts]
        front_addr = [f'{x}:{self.settings.front_port}' for x in hosts]
        tss_keys = []
        for i in range(nodes):
            tss_keys += [TSSKey.from_file(PathMaker.threshold_key_file(i))]
        ids = [x.id for x in tss_keys]
        mempool_addr = [f'{x}:{self.settings.mempool_port}' for x in hosts]
        committee = Committee(names, ids, consensus_addr,smvba_addr, front_addr, mempool_addr)
        committee.print(PathMaker.committee_file())

        node_parameters.print(PathMaker.parameters_file())

        # Cleanup all nodes.
        cmd = f'{CommandMaker.cleanup()} || true'
        g = Group(*hosts, user=self.settings.ssh_user, connect_kwargs=self.connect)
        g.run(cmd, hide=True)

        # Upload configuration files.
        def upload(i, c):
            c.put(PathMaker.committee_file(), '.')
            c.put(PathMaker.key_file(i), '.')
            c.put(PathMaker.threshold_key_file(i), '.')
            c.put(PathMaker.parameters_file(), '.')
        self._parallel(hosts, upload, prefix='Uploading config files:')

        return committee

    def _run_single(self, hosts, rate, bench_parameters, node_parameters, ts, debug=False):
        Print.info('Booting testbed...')

        # Kill any potentially unfinished run and delete logs.
        self.kill(hosts=hosts, delete_logs=True)

        Print.info('Killed previous instances')
        sleep(10)

        # Make dir
        cmd = f'{CommandMaker.make_logs_and_result_dir(ts)}'
        g = Group(*hosts, user=self.settings.ssh_user, connect_kwargs=self.connect)
        g.run(cmd, hide=True)
        
        # Run the clients (they will wait for the nodes to be ready).
        # Filter all faulty nodes from the client addresses (or they will wait
        # for the faulty nodes to be online).
        committee = Committee.load(PathMaker.committee_file())
        addresses = [f'{x}:{self.settings.front_port}' for x in hosts]
        rate_share = ceil(rate / committee.size())  # Take faults into account.
        timeout = node_parameters.timeout_delay
        node_sync = node_parameters.node_sync_delay
        client_logs = [PathMaker.client_log_file(i, ts) for i in range(len(hosts))]
        client_cmds = [
            CommandMaker.run_client(
                addr,
                bench_parameters.tx_size,
                rate_share,
                timeout,
                node_sync,
                nodes=addresses
            )
            for addr in addresses
        ]
        self._parallel(
            hosts, lambda i, c: self._background_run(c, client_cmds[i], client_logs[i])
        )

        Print.info('Clients boosted...')
        sleep(10)

        # Run the nodes.
        key_files = [PathMaker.key_file(i) for i in range(len(hosts))]
        dbs = [PathMaker.db_path(i) for i in range(len(hosts))]
        node_logs = [PathMaker.node_log_file(i, ts) for i in range(len(hosts))]
        threshold_key_files = [PathMaker.threshold_key_file(i) for i in range(len(hosts))]
        node_cmds = [
            CommandMaker.run_node(
                key_file,
                threshold_key_file,
                PathMaker.committee_file(),
                db,
                PathMaker.parameters_file(),
                debug=debug
            )
            for key_file, threshold_key_file, db in zip(key_files, threshold_key_files, dbs)
        ]
        self._parallel(
            hosts, lambda i, c: self._background_run(c, node_cmds[i], node_logs[i])
        )

        # Wait for the nodes to synchronize
        Print.info('Waiting for the nodes to synchronize...')

        # Wait for all transactions to be processed.
        duration = bench_parameters.duration
        for _ in progress_bar(range(100), prefix=f'Running benchmark ({duration} sec):'):
            sleep(duration / 100)
        self.kill(hosts=hosts, delete_logs=False)

    def _logs(self, hosts, faults, ddos, ts):
        # Download log files.
        def download(i, c):
            c.get(PathMaker.node_log_file(i, ts), local=PathMaker.node_log_file(i, ts))
            c.get(
                PathMaker.client_log_file(i, ts), local=PathMaker.client_log_file(i, ts)
            )
        self._parallel(hosts, download, prefix='Downloading logs:')

        # Parse logs and return the parser.
        Print.info('Parsing logs and computing performance...')
        return LogParser.process(PathMaker.logs_path(ts), faults=faults, ddos=ddos)

    def run(self, bench_parameters_dict, node_parameters_dict, debug=False):
        assert isinstance(debug, bool)
        Print.heading('Starting remote benchmark')
        try:
            bench_parameters = BenchParameters(bench_parameters_dict)
            node_parameters = NodeParameters(node_parameters_dict)
        except ConfigError as e:
            raise BenchError('Invalid nodes or bench parameters', e)

        # Select which hosts to use.
        selected_hosts = self._select_hosts(bench_parameters)
        if not selected_hosts:
            Print.warn('There are not enough instances available')
            return

        # Build once and upload binaries to the nodes.
        try:
            self._update(selected_hosts)
        except (OSError, subprocess.SubprocessError, GroupException) as e:
            e = FabricError(e) if isinstance(e, GroupException) else e
            raise BenchError('Failed to build or upload binaries', e)

        Print.info('Running Icarus')
        Print.info(f'{bench_parameters.faults} faults')
        Print.info(f'Timeout {node_parameters.timeout_delay} ms, Network delay {node_parameters.network_delay} ms')
        Print.info(f'DDOS attack {node_parameters.ddos}')

        # Run benchmarks.
        for n in bench_parameters.nodes:
            # Configure the full committee before omitting faulty nodes.
            hosts = selected_hosts[:n]
            try:
                self._config(hosts, node_parameters)
            except (OSError, subprocess.SubprocessError, GroupException, ConfigError) as e:
                e = FabricError(e) if isinstance(e, GroupException) else e
                raise BenchError('Failed to configure nodes', e)

            for r in bench_parameters.rate:
                Print.heading(f'\nRunning {n} nodes (input rate: {r:,} tx/s)')
                hosts = selected_hosts[:n]

                # Do not boot faulty nodes.
                faults = bench_parameters.faults
                hosts = hosts[:n-faults]

                ddos = node_parameters.ddos

                ts = datetime.now().strftime("%Y-%m-%d-%H-%M-%S")
                # Make dir
                cmd = CommandMaker.make_logs_and_result_dir(ts)
                subprocess.run([cmd], shell=True, stderr=subprocess.DEVNULL)
                
                # Run the benchmark.
                for i in range(bench_parameters.runs):
                    Print.heading(f'Run {i+1}/{bench_parameters.runs}')
                    try:
                        self._run_single(
                            hosts, r, bench_parameters, node_parameters, ts, debug
                        )
                        self._logs(hosts, faults, ddos, ts).print(PathMaker.result_file(
                            n, r, bench_parameters.tx_size, faults, ts
                        ))
                    except (subprocess.SubprocessError, GroupException, ParseError) as e:
                        self.kill(hosts=hosts)
                        if isinstance(e, GroupException):
                            e = FabricError(e)
                        Print.error(BenchError('Benchmark failed', e))
                        continue
