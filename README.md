[![rustc](https://img.shields.io/badge/rustc-1.50.0+-blue?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![python](https://img.shields.io/badge/python-3.9-blue?style=flat-square&logo=python&logoColor=white)](https://www.python.org/downloads/release/python-390/)
[![license](https://img.shields.io/badge/license-Apache-blue.svg?style=flat-square)](LICENSE)

# Icarus

The emergence of blockchain technology has revitalized research interest in Byzantine Fault Tolerant (BFT) consensus, particularly asynchronous BFT. To improve the performance of traditional asynchronous BFT, recent studies propose the dual-path paradigm, but owing to the inherent complexity and inefficiency of the Multi-valued Validated Byzantine Agreement (MVBA) protocol, existing dual-path protocols exhibit high implementation complexity and poor performance in unfavorable situations. 

To address these limitations, we propose **Icarus**, a **single-path asynchronous BFT protocol** that exclusively leverages optimistic paths without relying on pessimistic paths.

## Environment Setup

You have two options to set up the development environment:

### Option 1: Using Docker (Recommended)

If you prefer not to manually configure the development environment, you can use the provided Dockerfile which includes all necessary dependencies:

```bash
# Build the Docker image
docker build -t icarus .

# Run the container
docker run -it --name icarus-dev icarus /bin/bash
```

For Alibaba Cloud benchmarks, use the additional read-only credential mounts
documented under [Docker credential mounts](#docker-credential-mounts).

The Docker image includes:
- Rust toolchain with nightly version
- RISC-V cross-compilation tools
- All development dependencies (clang, tmux, cmake, etc.)
- Python 3 and other required tools

### Option 2: Manual Setup

Alternatively, you can manually install the required dependencies on your local machine. You'll need:
- Rust (nightly version)
- Clang
- tmux
- Python 3.9+
- Other development tools as specified in the Dockerfile

## Quick Start

### 

Icarus is written in Rust, but all benchmarking scripts are written in Python and run with [Fabric](http://www.fabfile.org/).
To deploy and benchmark a testbed of 4 nodes on your local machine, clone the repo and install the python dependencies:

```
$ git clone https://github.com/DGJGzy/Icarus
$ cd Icarus/benchmark
$ pip install -r requirements.txt
```

You also need to install Clang (required by rocksdb) and [tmux](https://linuxize.com/post/getting-started-with-tmux/#installing-tmux) (which runs all nodes and clients in the background). 

Then, compile the entire project:

```
$ cargo build
```

Finally, run a local benchmark using fabric:

```
$ fab local
```

This command may take a long time the first time you run it (compiling rust code in `release` mode may be slow) and you can customize a number of benchmark parameters in `fabfile.py`. When the benchmark terminates, it displays a summary of the execution similarly to the one below.

```
Starting local benchmark
Setting up testbed...
Running Icarus
0 faults
Timeout 1000 ms, Network delay 10000 ms
DDOS attack False
Waiting for the nodes to synchronize...
Running benchmark (30 sec)...
Parsing logs...

-----------------------------------------
 SUMMARY:
-----------------------------------------
 + CONFIG:
 DDOS attack: False 
 Committee size: 4 nodes
 Input rate: 10,000 tx/s
 Transaction size: 256 B
 Faults: 0 nodes
 Execution time: 30 s

 Consensus timeout delay: 1,000 ms
 Consensus sync retry delay: 10,000 ms
 Consensus max payloads size: 1,000 B
 Consensus min block delay: 0 ms
 Mempool queue capacity: 10,000 B
 Mempool max payloads size: 256,000 B
 Mempool min block delay: 0 ms
 Lambda: 10

 + RESULTS:
 Consensus TPS: 10,010 tx/s
 Consensus BPS: 2,562,529 B/s
 Consensus latency: 8 ms

 End-to-end TPS: 10,009 tx/s
 End-to-end BPS: 2,562,276 B/s
 End-to-end latency: 13 ms
-----------------------------------------
```

## Alibaba Cloud Benchmarks

The original Rust benchmark now uses ECS through `benchmark/alibaba/`; EC2 is
no longer supported. Rust binaries, independent clients, one node per machine,
fault semantics, and latency/throughput formulas are retained. The separate
`benchmark-falcon/` framework is not used.

### Prerequisites and credentials

Use Python 3.10, a local Rust toolchain, and the following Python dependencies.
Python 3.11+ is not supported: the pinned Fabric 2.6.0 depends on invoke 1.x,
which calls the removed `inspect.getargspec`.

```shell
cd benchmark
python -m pip install -r requirements.txt
fab --list
```

Use a dedicated RAM identity permitted to call ECS `DescribeInstances`,
`DescribeImages`, `DescribeAvailableResource`, `RunInstances`, `StartInstances`,
`StopInstances`, `DeleteInstances`, `DescribeSecurityGroups`,
`DescribeSecurityGroupAttribute`, `CreateSecurityGroup`, `AuthorizeSecurityGroup`,
and `DeleteSecurityGroup`, including resource tagging during creation. Automatic
network preparation also requires VPC `DescribeVpcs`, `DescribeVpcAttribute`,
`DescribeVSwitches`, `DescribeVSwitchAttributes`, `CreateVpc`, and `CreateVSwitch`,
including tags on newly created networks. Restrict permissions to the intended
resources/regions.
Account policies, quota, and zone capacity must also permit the requested instances.

The configuration points `key.accesskey` to `~/.aliyun/access.json`, a private JSON
file with Falcon-compatible fields `"AccessKey ID"` and `"AccessKey Secret"`.
Store it outside this repository with mode 600; never share its contents.
Alternatively, omit `key.accesskey` and set `ALIBABA_CLOUD_ACCESS_KEY_ID` and
`ALIBABA_CLOUD_ACCESS_KEY_SECRET` in your local environment.
Neither `fab --list` nor `fab local` reads cloud credentials.

#### Docker credential mounts

Inside the default root container, `~` resolves to `/root`, not your host home.
Mount both the AccessKey directory and the configured SSH key directory; mounting
only the repository does not make either credential available.

Run this on the **host**, from the repository root, after ensuring
`~/.aliyun/access.json` and `~/.ssh/Aliyun/id_ed25519` exist:

```shell
docker run -it --name icarus-cloud \
  --mount "type=bind,src=$PWD,dst=/root/workspace/Jcarus" \
  --mount "type=bind,src=$HOME/.aliyun,dst=/root/.aliyun,readonly" \
  --mount "type=bind,src=$HOME/.ssh/Aliyun,dst=/root/.ssh/Aliyun,readonly" \
  --workdir /root/workspace/Jcarus/benchmark \
  icarus /bin/bash
```

Docker cannot add bind mounts to an existing container with `docker start` or
`docker exec`. The command above uses a new container name so the existing one
can be kept. Packages installed only inside the old container are not in the
image; install the Python dependencies in the new container as described above.
Do not copy credentials into the image or repository, or mount your entire home.

Inside the new container, check the SSH key is readable and initialize the cloud
clients without calling Alibaba Cloud APIs or creating billable resources:

```shell
test -r /root/.ssh/Aliyun/id_ed25519 && \
  python3 -c "from alibaba.instance import InstanceManager; InstanceManager.make(); print('Alibaba Cloud clients initialized')"
```

### Required configuration

The configuration follows Falcon's instance type, four regions, and public
ingress rules, but prepares networks automatically instead of depending on its
hard-coded `aleabft` VPC. AccessKey contents are not stored in the repository.

| Field | What to supply |
|---|---|
| `key.name` | The same SSH key-pair name imported into every configured ECS region. |
| `key.path` | Local RSA or Ed25519 private-key path. `~` is expanded; relative paths are resolved beside settings.json. |
| `key.accesskey` | Optional private credentials-file path, with the same path resolution rules. |
| `ssh.user` | Actual SSH login user; default root. Other users need non-interactive sudo. |
| `instances.name` | Unique, stable testbed identifier; do not share it across experiments. |
| `instances.type` | Available x86_64 ECS instance type; the Falcon setting is ecs.g9i.xlarge. |
| `instances.regions` | Non-empty list of unique Alibaba Cloud region IDs. VPCs, vSwitches, and images are selected automatically. |
| `ports` | Defaults: consensus 8000, SMVBA 9000, mempool 7000, front/client 6000. |
| `repo.name`, `repo.url`, `repo.branch` | Remote checkout directory, repository reachable from ECS, and branch to measure. Check the example values: local unpushed changes are not deployed. |

System disks and public bandwidth use fixed defaults in `InstanceManager`:
200 GiB `cloud_essd` and 100 Mbit/s. They are not configurable through
`settings.json`; no disk/bandwidth fields or `security` object are required.

`instances.regions` lists the four configured regions directly:

```json
["us-east-1", "ap-southeast-1", "us-west-1", "eu-central-1"]
```

`fab create` prepares every region's network before creating any instances or
security groups. It creates/reuses a VPC named after `instances.name`.
Multiple matching VPCs cause an error rather than an arbitrary selection;
rename duplicate VPCs before proceeding. A missing VPC is created with
IPv4 CIDR `10.0.0.0/16`.

The adapter queries available zones for the configured instance type and fixed
ESSD system disks. It reuses a ready vSwitch in a compatible zone with enough
free IP addresses, or creates one there using a non-overlapping subnet of the
VPC's primary IPv4 CIDR. New subnets normally use `/24`, grow for more than 252
instances, and can be smaller when the VPC itself is smaller. VPCs and vSwitches
must finish provisioning before ECS creation proceeds. No compatible zone or
insufficient subnet space causes an error; stock checks do not reserve capacity.
Subsequent runs discover and reuse the network. Newly created networks carry
`JcarusTestbed=<instances.name>` tags; reused networks are not retagged.

Resolved VPC/vSwitch IDs are held in runtime state, not written into
`settings.json`. An official Ubuntu 22.04 x86_64 system image is discovered
automatically in each region.

Automatic networks in different regions may use the same private CIDR: this
benchmark communicates across regions via public IPs, not private VPC peering.
No NAT gateway or cross-region private-network connection is created.

The configured SSH key-pair name is `ymbmzy`. Before creating ECS instances,
import the public key from `~/.ssh/Aliyun/id_ed25519.pub` under that name in
every configured region. `key.name` is the cloud resource name, not a local
filename: `key.path` remains `~/.ssh/Aliyun/id_ed25519` unless the private key
itself changes. AccessKey credentials are independent of this SSH key-pair name.

Changing `key.name` affects newly created instances only; it does not rebind
keys on existing ECS instances. The adapter does not import SSH key pairs
automatically. Read-only verification of cloud key pairs requires
`ecs:DescribeKeyPairs` permission.

The adapter creates/reuses tagged security groups and authorizes the four
original Rust protocol ports, not Falcon's consensus-only port range.
Check host firewalls and cross-region public-IP connectivity as well.

The adapter uses a fixed `0.0.0.0/0` ingress default for SSH and all four protocol
ports, matching Falcon: these ports are exposed to the Internet. This is intended
only for isolated experiments. For a restricted deployment, change the adapter's
ingress default before creating resources and inspect existing rules. Reused
groups retain extra rules: changing the default does not revoke old broad rules.
Remove those rules in the console or destroy/recreate the testbed.

### Run and clean up

Run commands from `benchmark/`. `create --nodes=N` creates N machines **per
region**, one Rust node and one client per machine. Creation incurs instance,
disk, and public-traffic charges.

```shell
fab create --nodes=7
fab info
# Verify a printed SSH command and complete peer ingress rules.
# Then, on the collector (fab collector-ssh, see below):
fab install
fab remote
```

Run `fab install` and `fab remote` on the collector. `fab install` only waits
for cloud-init and makes sure tmux is present on each node. `fab remote`
fast-forwards the collector's repository to `repo.branch`, compiles it once
there, and uploads the `node`/`client` binaries to the selected nodes, skipping
nodes whose binaries are unchanged. Nodes need no Rust toolchain or Git access.
The collector must reach Ubuntu package mirrors, Rust servers, Cargo/Git
dependencies, and the repository. Arrange private-repository authentication
separately; AccessKeys do not grant Git access. `fab remote` refuses to run on
machines other than Linux x86_64, such as macOS.

Configure experiment size/rate/duration/faults in the `remote` task in
`fabfile.py`. Its current default needs at least four running, publicly
addressable machines. Regions are selected round-robin; each node-count setting
gets its own committee. Logs/results remain in `logs/<timestamp>/` and
`results/<timestamp>/`.

```shell
fab kill            # Stop benchmark processes, not instances or billing.
fab stop            # Stop ECS instances, retaining public IP allocation.
fab start --max=7   # Start at most seven stopped instances per region.
fab destroy         # Delete this testbed's tagged instances/security groups.
```

`stop` uses ECS KeepCharging: **stopped instances can continue to incur charges**.
Use `destroy` when finished and inspect the console for remaining resources.
Management is restricted to `JcarusTestbed=<instances.name>` in the configured
regions. VPCs and vSwitches, including automatically created ones, are retained
for reuse and must be deleted manually when no longer needed. Unrelated resources
and independently retained disks/snapshots are not deleted. Do not change the
testbed name or remove regions before cleaning up the old testbed.

### Independent collector/controller

Run the existing benchmark from an independent ECS machine, keeping raw logs
and parsing in the cloud. Only result files are downloaded to the local machine.
The collector defaults to Hong Kong (`cn-hongkong`), outside the four experiment
regions, with `ecs.g9i.xlarge`, Ubuntu 22.04, a 200 GiB ESSD system disk, and
100 Mbit/s public egress. Availability is checked at creation; there is no
automatic region/type substitution.

Configure the `collector` section in `benchmark/settings.json`. Its `name`
identifies exactly one collector; `region` must not appear in `instances.regions`.
The separate `JcarusCollector=<collector.name>` tag and SSH-only security group
exclude it from the normal consensus-node `create/start/stop/destroy` commands.
Resources carrying both collector and consensus tags are never adopted.
Keep the experiment regions and `instances.name` unchanged.

`collector.ssh_cidr` defaults to `0.0.0.0/0`, exposing only SSH. Restrict it to
your local public IP/CIDR before creation when possible. Existing security-group
rules are not automatically revoked. Allow outbound API/package/Git access and
SSH from the collector to the experiment nodes; no consensus ports are needed
on the collector itself.

Run these commands locally from `benchmark/`, with the project's Python 3.10
environment activated and system `ssh`, `ssh-keygen`, and `rsync` available:

```shell
fab collector-create
fab collector-info
fab collector-setup
fab collector-ssh
```

`collector-create` reuses the unique matching instance, starting it if stopped;
it does not create a second instance. Creation and starting incur charges.
Before creating a new instance it derives the public key from `key.path`,
checks the same `key.name` in the collector region, and imports it if absent.
A different/unverifiable existing public key is an error, not overwritten.
Local cloud credentials therefore also need Hong Kong access and ECS
`DescribeKeyPairs`/`ImportKeyPair` permissions, in addition to the cloud
management/network permissions above. Creation stock checks are not reservations.

`collector-setup` installs the Rust toolchain, deploys the configured Jcarus
repository at `~/Jcarus`, and installs Python 3.10, rsync, and `~/Jcarus/.venv`
with `benchmark/requirements.txt`. Current local benchmark Python scripts are
uploaded too, so new commands do not require a Git push. Rust sources still come
from `repo.url`/`repo.branch`: push Rust changes and `fab remote` picks them up.
Uploaded scripts/settings stay as local changes on the collector; `fab remote`
never discards them and stops if a pushed commit conflicts with them. Rerun
`fab collector-setup` in that case.
The sample repository configuration points to `YMbmzy/Jcarus`.

Setup copies exactly the configured `key.path` and `key.accesskey` files over
SSH/SFTP to the remote user's `~/.ssh/Aliyun/id_ed25519` and
`~/.aliyun/access.json`. It requires both files; environment-only AccessKeys are
not sufficient for this command. Private directories use mode 700, and atomic
credential uploads use mode 600. The remote settings retain `key.name`, the
four experiment regions and testbed name, with rewritten credential paths.
Repeat setup redeploys local benchmark scripts/settings but preserves results.
The collector now holds the same credentials and effective cloud permissions
as the local machine; RAM source-IP restrictions still apply. Never use an
untrusted collector or put these files in Git.

Confirm the collector SSH host fingerprint on first access; changed keys are
not silently accepted. Setup verifies `DescribeInstances` in every experiment
region and SSH to discovered running nodes. If none are running it explicitly
reports that node SSH was not verified. Node host keys use trust-on-first-use
and reject subsequent changes. Encrypted SSH keys require an available unlocked
key on the collector for non-interactive node access.

`collector-ssh` enters `~/Jcarus/benchmark` with the virtualenv and Rust
environment loaded. Run experiments inside tmux so SSH disconnection does not
interrupt the controller:

```shell
tmux new -s jcarus
cd ~/Jcarus/benchmark
. ../.venv/bin/activate
. "$HOME/.cargo/env"
fab info
fab install
fab remote
```

Back on the local machine, choose either download mode:

| Command | Local destination and behavior |
|---|---|
| `fab collector-download` | New UTC timestamp/random directory under `benchmark/collector-results/downloads/` on every invocation; previous downloads are never overwritten. Failed snapshots are removed. |
| `fab collector-sync` | `benchmark/collector-results/current/`; checksum-different remote files overwrite local files, identical files are skipped, local-only files are retained. |
| `fab collector-download --directory=/path/to/archive` | Create a unique child under the supplied archive root. |
| `fab collector-sync --directory=/path/to/current` | Incrementally update the supplied directory itself. |

Both modes pull only `~/Jcarus/benchmark/results/`, never upload local results,
never propagate deletion, and reject destinations overlapping the original local
`benchmark/results/` or credential directories. Default local paths are anchored
beside settings.json and ignored by Git. Interrupted incremental downloads may
leave already transferred files; rerunning transfers remaining differences.
Downloads during an experiment capture only currently written data; download
after it finishes for complete results. The remote sender helper is deployed
by setup and also supports macOS's system rsync.

```shell
fab collector-destroy
```

Destruction requires typing the displayed collector instance ID (or collector
name when only orphan security groups remain). Download results first: remote
logs/results/credentials may be lost. Only collector-tagged instances and
security groups are removed; consensus resources and local downloads are kept.
VPCs/vSwitches and independently retained disks/snapshots remain; inspect the
console for retained resources and billing. Normal `fab destroy` intentionally
does not release the collector.

Instance deletion and security-group detachment are asynchronous. Destruction
waits up to 10 minutes per region for security-group dependencies to clear;
unrelated API errors still fail immediately. If an earlier attempt failed after
deleting the instance, rerun `fab collector-destroy` to remove orphan collector
security groups.

## Project Structure

This project is organized into several modules, each serving a specific purpose in the Byzantine Fault Tolerance (BFT) consensus protocol implementation:

### Core Modules

- **`consensus/`** - Core consensus protocol implementation
  - Contains the main BFT consensus logic
  - Implements commit rules, path switch, aggregation, synchronization, and message handling
  - Key files: `core.rs` (main consensus engine), `aggregator.rs` (vote aggregation), `synchronizer.rs` (message retrieve)

- **`mempool/`** - Transaction mempool management
  - Handles transaction buffering, ordering, and payload management
  - Provides interface between consensus layer and transaction pool
  - Key files: `mempool.rs` (main mempool logic), `core.rs` (transaction processing)

- **`network/`** - Network communication layer
  - Manages TCP connections, message serialization/deserialization
  - Handles peer-to-peer communication between nodes
  - Implements reliable message delivery and connection management

- **`crypto/`** - Cryptographic primitives and services
  - Provides digital signatures, hash functions, and threshold signatures
  - Implements Ed25519 signatures and threshold cryptography support
  - Key components: signature service, key generation, batch verification

- **`store/`** - Persistent storage layer
  - Built on RocksDB for reliable data persistence
  - Provides async read/write operations with notification system
  - Handles block storage, state persistence, and recovery

- **`node/`** - Main node implementation
  - Orchestrates all modules and runs the complete BFT node
  - Handles configuration, initialization, and inter-module communication
  - Entry point: `main.rs` for running a consensus node

### Benchmarking and Testing

- **`benchmark/`** - Performance benchmarking tools
  - Python-based benchmarking framework using Fabric
  - Supports both local and Alibaba Cloud ECS deployments
  - Provides comprehensive performance metrics and analysis
  - Key files: `fabfile.py` (benchmark commands), `benchmark/local.py` (local testing), `alibaba/` (cloud deployment)

### Configuration and Build

- **`Cargo.toml`** - Rust workspace configuration defining all modules
- **`dockerfile`** - Docker container setup for consistent development environment
- **`benchmark/settings.json`** - Alibaba Cloud deployment and testing configuration

Each module is designed to be modular and testable, with comprehensive test suites located in the `tests/` directories within each module. The project uses Rust's async/await for high-performance concurrent operations and implements state-of-the-art BFT consensus algorithms.

## License

This software is licensed as [Apache 2.0](./LICENSE).

## Reference

- [ParBFT](https://dl.acm.org/doi/10.1145/3576915.3623101)
- [Jolteon and Ditto](https://arxiv.org/pdf/2106.10362)
- [Speeding Dumbo](https://eprint.iacr.org/2022/027)
- [Tusk](https://dl.acm.org/doi/10.1145/3492321.3519594)
- [Fin](https://dl.acm.org/doi/10.1145/3576915.3616633)
