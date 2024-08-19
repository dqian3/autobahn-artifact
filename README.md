# Autobahn: Seamless high speed BFT - SOSP24 Artifact 
This is the repository for the Artifact Evaluation of SOSP'24 proceeding: "Autobahn: Seamless high speed BFT".

For all questions about the artifact please e-mail Neil Giridharan <giridhn@berkeley.edu> and Florian Suri-Payer <fsp@cs.cornell.edu>. 


# Table of Contents
1. [Artifact Overview](#artifact)
2. [High Level Claims](#Claims)
3. [Overview of steps to validate Claims](#validating)
4. [Installing Dependencies and Building Binaries](#installing)
5. [Setting up Cloud Environment](#cloud)
6. [Running Experiments](#experiments)


## Artifact Overview <a name="artifact"></a>

This artifact contains, and allows to reproduce, experiments for all figures included in the paper "Autobahn: Seamless high speed BFT". 

It contains a prototype implemententation of Autobahn, as well as the reference implementations used to evaluate baseline systems: VanillaHS, BatchedHS, and Bullshark. Each prototype is located on its *own* branch, named accordingly. Please checkout the corresponding branch when validating claims for a given system.


Autobahn and all baseline systems are implemented in Rust, using the asynchronous Tokio runtime environment. TCP is used for networking, and ed25519-dalek signatures are used for authentication.
Replicas persist all messages receives to disk, using RocksDB.
Client processes connect to *local* Replica machines and submit dummy payload requests (transactions) only to this replica. Replicas distribute payloads to one another -- the specifics depend on the particular system. 

Orienting oneself in the code: 
Autobahn/Bullshark: The two main modules are "worker" and "primary". The worker layer is responsible for receiving client requests. It forwards data and digests to the primary layer which contains the main consensus logic.
VanillaHS/BatchedHS: The core module is "consensus"

TODO: Describe also what it doesnt do.
- e.g. do we have exponential timeouts?



## Concrete claims in the paper
Autobahn is a Byzantine Fault Tolerant (BFT) consensus protocol that aims to hit a sweet spot between high throughput, low latency, and the ability to recover from asynchrony (seamlessness).

- **Main claim 1**: Autobahn matches the Throughput of Bullshark, while reducing latency by a factor of 2x. 

- **Main claim 2**: Autobahn avoids hangovers in the presence of blips.



## Validating the Claims - Overview <a name="validating"></a>

All our experiments were run using Google Cloud Platform (GCP) (TODO: add link). To reproduce our results and validate our claims, you will need to 1) instantiate a matching GCP experiment, 2) build the prototype binaries, and 3) run the provided experiment scripts with the (supplied) configs we used to generate our results.

The ReadMe is organized into the following high level sections:

1. *Installing pre-requisites and building binaries*

   To build Autobahn and baseline source code in any of the branches several dependencies must be installed. Refer to section "Installing Dependencies" for detailed instructions on how to install dependencies and compile the code. 

2. *Setting up experiments on GCP* 

     To re-run our experiments, you will need to instantiate a distributed and replicated server (and client) configuration using GCP. 
     <!-- We have provided a public profile as well as public disk images that capture the configurations we used to produce our results. Section "Setting up Cloudlab" covers the necessary steps in detail. Alternatively, you may create a profile of your own and generate disk images from scratch (more work) - refer to section "Setting up Cloudlab" as well for more information. Note, that you will need to use the same Cluster (Utah) and machine types (m510) to reproduce our results. -->


3. *Running experiments*

     To reproduce our results you will need to checkout the respective branch, and run the supplied experiment scripts using the supplied experiment configurations. Section "Running Experiments" includes instructions for using the experiment scripts, modifying the configurations, and parsing the output. 
     

## Installing Dependencies <a name="installing"></a>
TODO: What hardware req. What software env (ubuntu). What installs (Rust, cargo, tokio, rocks. etc..?)

Software environment: Ubuntu 20.04 Focal
Packages: `clang` (version <= 14, do not use version 15 or higher), `tmux`, `rust` (stable 1.80)

Building code: 
`cargo build`

## Testing Locally
i.e. quick local run to get some numbers/see that it works (this might already clear the bar for some of the badges)
In order to run a quick test locally simple navigate to `benchmark/` and run `fab local`

This will run a simple local experiment, using the parameters provided in `fabfile.py` (in `def local()`)
Additional instructions can be found in `benchmark/README`.
> [!WARNING]
> The Readme in branches Autobahn and Bullshark also contains some instructions to run on AWS. 
> These are inherited from Narwhal/Bullshark and have NOT BEEN TESTED by us. 
> We recommend you use the GCP instructions that we have trialed ourselves.


## Setting up GCP
TODO: Can we provide alternatives on how to run elsehwere? (if the artifact committee cannot run on GCP)

Detail which machines and configs we used (CPU/SSD...). What geo setup (i.e. where machines are located)

We recommend running on GCP as our experiment scripts are designed to work with GCP. 
New users to GCP can get $300 worth of free credit (https://console.cloud.google.com/welcome/new), which should be sufficient to reproduce our core results. The Google Cloud console is the gateway for accessing all GCP services. Most of the time we will use the compute engine service to create and manage VMs but occassionally we will use other services. You can search for other services using the GCP console searchbar.

### Creating a project
The first step is to create a compute engine project. Follow the instructions here:
https://developers.google.com/workspace/guides/create-project
We recommend you name the project autobahn but you can choose any name you like.

### Setup SSH keys
In order to connect to GCP you will need to register an SSH key.

If you do not already have an ssh key-pair, run the following command locally to generate ssh keys.
`ssh-keygen -t rsa -f ~/.ssh/KEY_FILENAME -C USERNAME -b 2048`

To add a public SSH key to the project metadata using the Google Cloud console, do the following:

1. In the Google Cloud console, go to the Metadata page.

2. Click the SSH keys tab.

3. Click Edit.

4. Click Add item.

5. In the SSH key field that opens, add your public SSH key. The key must be in one of the following formats:

`KEY_VALUE USERNAME`

Replace the following:

KEY_VALUE: the public SSH key value
USERNAME: your username. For example, cloudysanfrancisco or cloudysanfrancisco_gmail_com.
For Linux VMs, the USERNAME can't be root, unless you configure your VM to allow root login. For more information, see Connect to Linux VMs as the root user.

For Windows VMs that use Active Directory (AD), the username must be prepended with the AD domain, in the format of DOMAIN\. For example, the user cloudysanfrancisco within the ad.example.com AD has a USERNAME of example\cloudysanfrancisco.

6. Click Save.

### Setting up Google Virtual Private Cloud (VPC)
Next, you will need to create your own Virtual Private Cloud network. To do so: 

1. In the Google Cloud console, go to the VPC networks page.

2. Click Create VPC network.

3. Enter a Name for the network (we recommend `autobahn-vpc`).

4. Maximum transmission unit (MTU): Choose 1460 (default)

5. Choose Automatic for the Subnet creation mode.

6. In the Firewall rules section, select zero or more predefined firewall rules. The rules address common use cases for connectivity to instances.

Whether or not you select pre-defined rules, you can create your own firewall rules after you create the network.

Each predefined rule name starts with the name of the VPC network that you are creating, NETWORK. In the IPv4 firewall rules tab, the predefined ingress firewall rule named NETWORK-allow-custom is editable. By default it specifies the source range 10.128.0.0/9, which contains current and future IPv4 ranges for subnets in an auto mode network. The right side of the row that contains the rule, click Edit to select subnets, add additional IPv4 ranges, and specify protocols and ports.

7. Choose the Dynamic routing mode for the VPC network.

8. Click Create.

9. Check that the default 4 firewall rules are there (see `Pre-populated rules in the default network` here https://cloud.google.com/firewall/docs/firewalls)

### Create Instance Templates
We're now ready to create an Instance Template for each region we need, containing the respective hardware configurations we will use.

We used the following four regsions in our experiments: us-east5, us-east1, us-west1, us-west4. 
Create one instance template per region as follows:

1. In the Google Cloud console, go to the Instance templates page.

2. Click Create instance template.

3. Give instance template a name

4. Select the Location as follows:
Choose Regional.
Select the Region where you want to create your instance template.

5. Select a Machine type.
Choose t2d-standard-16 (16vCPU, 64 GB of memory) (under General purpose category)
Choose Spot for VM provisioning model (to save costs)
Choose 20 GB balanced persistent disk (in the Boot disk section). This is important! If you use a HDD then writing to disk may become a bottleneck.
Select ubuntu-2004-focal-v20231101 as the Image

6. Click Create to create the template.

Create one last instance template to serve as the control machine. Pick any of the four regions. Name this instance template `autobahn-instance-template` (the scripts assume this is the name of the control machine).
For this instance template select Standard instead of Spot for VM provisioning model (so it won't be pre-empted while running an experiment).
We recommend you pick t2d-standard-4 (instead of t2d-standard-16) for the machine type for the control machine to save costs.

## Running Experiment

i.e. what scripts to run, what configs to give, and how to collect/interpret results.
-> fab remote

The GCP config is found in `settings.json`. You will need to change the following:
1. `key`: change the `name` (name of the private SSH key) and `path` fields to match the key you generated in the prior section
The `port` field will remain the same (value of 5000).
2. `repo`: The `name` field will remain the same (value of autobahn-artifact). You will need to change the `url` field to be the url of the artifact github repo. Specifically, you will need to prepend your personal access token to the beginning of the url. The url should be in this format: "https://TOKEN@github.com/neilgiri/autobahn-artifact", where `TOKEN` is the name of your personal access token. `branch` specifies which branch will be run on all the machines. This will determine which system ends up running.
3. `project_id`: change this to be the name of the project id you created in the prior section
4. `instances`: `type` (value of t2d-standard-16) and `regions` (value of ["us-east1-b", "us-east5-a", "us-west1-b", "us-west4-a"])will remain the same. If you select different regions then you will need to change the regions field to be the regions you are running in. You will need to change `templates` to be the names of the instance templates you created. The order matters, as they should correspond to the each region. The path should be in the format "projects/PROJECT_ID/regions/REGION_ID/instanceTemplates/TEMPLATE_ID", where PROJECT_ID is the id of the project you created in the prior section, REGION_ID is the name of the region without the subzone (i.e. us-east1 NOT us-east1-a).

When running for the first time,  run `fab create` which will create machines based off your instance templates.
Then run `fab install` which will install rust and the dependencies on these machines.
Finally `fab remote` will launch a remote experiment. For subsequent experiments you can skip running `fab create` and `fab install`

## Configuring Parameters
The parameters for the remote experiment are found in `fabfile.py`. To change the parameters locate the remote task in `fabfile.py`. This task specifies two types of parameters, the benchmark parameters and the nodes parameters. The benchmark parameters look as follows:

`bench_params = {
    'nodes': 4,
    'workers': 1,
    'rate': 50_000,
    'tx_size': 512,
    'faults': 0,
    'duration': 20,
}`

They specify the number of primaries (nodes) and workers per primary (workers) to deploy, the input rate (tx/s) at which the clients submits transactions to the system (rate), the size of each transaction in bytes (tx_size), the number of faulty nodes ('faults), and the duration of the benchmark in seconds (duration). The minimum transaction size is 9 bytes, this ensure that the transactions of a client are all different. The benchmarking script will deploy as many clients as workers and divide the input rate equally amongst each client. For instance, if you configure the testbed with 4 nodes, 1 worker per node, and an input rate of 1,000 tx/s (as in the example above), the scripts will deploy 4 clients each submitting transactions to one node at a rate of 250 tx/s. When the parameters faults is set to f > 0, the last f nodes and clients are not booted; the system will thus run with n-f nodes (and n-f clients).

The nodes parameters determine the configuration for the primaries and workers:

`node_params = {
    'header_size': 1_000,
    'max_header_delay': 100,
    'gc_depth': 50,
    'sync_retry_delay': 10_000,
    'sync_retry_nodes': 3,
    'batch_size': 500_000,
    'max_batch_delay': 100,
    'use_optimistic_tips': True,
    'use_parallel_proposals': True,
    'k': 4,
    'use_fast_path': True,
    'fast_path_timeout': 200,
    'use_ride_share': False,
    'car_timeout': 2000,
    
    'simulate_asynchrony': True,
    'asynchrony_type': [3],
    'asynchrony_start': [10_000], #ms
    'asynchrony_duration': [20_000], #ms
    'affected_nodes': [2],
    'egress_penalty': 50, #ms
    
    'use_fast_sync': True,
    'use_exponential_timeouts': False,
}`
They are defined as follows:

*header_size: The preferred header size. The primary creates a new header when it has enough parents and enough batches' digests to reach header_size. Denominated in bytes.
*max_header_delay: The maximum delay that the primary waits between generating two headers, even if the header did not reach max_header_size. Denominated in ms.
*gc_depth: The depth of the garbage collection (Denominated in number of rounds).
*sync_retry_delay: The delay after which the synchronizer retries to send sync requests. Denominated in ms.
*sync_retry_nodes: Determine with how many nodes to sync when re-trying to send sync-request. These nodes are picked at random from the committee.
*batch_size: The preferred batch size. The workers seal a batch of transactions when it reaches this size. Denominated in bytes.
*max_batch_delay: The delay after which the workers seal a batch of transactions, even if max_batch_size is not reached. Denominated in ms.
*use_optimistic_tips: Whether to enable optimistic tips optimization. If set to true then non-certified proposals can be sent to consensus
*use_parallel_proposals: Whether to allow multiple active consensus instances at a time in parallel
*k: The maximum number of consensus instances allowed to be active at any time.
*use_fast_path: Whether to enable the 3f+1 fast path for consensus
*fast_path_timeout: The timeout for waiting for 3f+1 responses on the consensus fast path
*use_ride_share: Whether to enable the ride-sharing optimization of piggybacking consensus messages on car messages
*car_timeout: The timeout for sending a car
*simulate_asynchrony: Whether to allow blips
*asynchrony_type: The specific type of blip.
*asynchrony_start: The start times for each blip event
*asynchrony_duration: The duration of each blip event
*affected_nodes: How many nodes experience blip behavior
*egress_penalty: For egress blips how much egress delay is added
*use_fast_sync: Whether to enable the fast sync optimization. If set to False the recursive sync strategy will be used
*use_exponential_timeouts: Whether to enable timeout doubling upon timeouts firing



The configs for each experimented are located the `experiment_configs` folder. To run a specific experiment copy and paste the experiment config into the fab remote task. For all experiments besides the scaling experiment you will want to make sure `nodes=1`. This will create 1 node per region specificed in the settings.json file.


Explain what parameters to configure in config and what they control. 

Explain what file to look at for results, and which lines/numbers to look for.

## Reproducing Results
Provide ALL configs for each experiment. But suggest they only validate the claims.
The experiment configs

### Performance under ideal conditions
When an experiment finishes the logs and output files are downloaded to the control machine. The performance results are found in `results/bench-

#### Autobahn
Peak throughput is around 234k txn/s, end-to-end latency is around 280 ms. The config to get the peak throughput is found in `autobahn-peak.txt`.

#### Bullshark

#### Batched-HS

#### Vanilla-HS

- for each system, give our peak numbers + the config. Have them reproduce those

Exp 2
- for each n, and each system give the numbers (i.e. the whole fig as a table)

Exp 3
- show the 3s blip in HS, and lack thereof for us (don't think we need to show the other two blips).
- give the config. Explain how to interpret the data file to see blip duration and hangover duration (Be careful to explain that the numbers can be slightly offset)

Exp 4
- give the configs and run all. Same same.
