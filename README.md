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

We provided an install script `install_deps.sh` in the `overview` branch which we recommend for installing any necessary dependencies.

The high-level requirements for compiling Autobahn and the baselines are:
-Operating System: Ubuntu 20.04, Focal
    - While it should compile on any operating system, we recommend running on Ubuntu 20.04 as that is what we have tested with
- Requires python3
- Requuires rust (recommend 1.80 stable)
- Requires clang version <= 14 (for building librocksdb, DO NOT use version 15 or higher)
- tmux

Before beginning the install process make sure to update your distribution:
1. `sudo apt-get update`

If not using `install_deps.sh` make sure to use the script here: https://bootstrap.pypa.io/get-pip.py and not apt-get, and update the `PATH` environment variable to point to the location of pip.

After installation finishes, navigate to `autobahn-artifact/benchmark` and run `pip install -r requirements.txt`.

### Building code: 
Finally, you can build the binaries (you will ned to do this anew on each branch):
Navigate to `autobahn-artifact` directory and build:
-`cargo build`

## Testing Locally
i.e. quick local run to get some numbers/see that it works (this might already clear the bar for some of the badges)
In order to run a quick test locally:
1. checkout the branch `autobahn-simple-sender` (for a basline checkout the appropriate branch instead)
2. navigate to `autobahn-artifact/benchmark/`
3. run `fab local`.

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

1. Click the Try For Free blue button
2. Enter your account information for step 1 and step 2
3. Click the Start Free blue button after step 2
4. Optionally complete the survey
5. Creating an account should automatically create a project called "My First Project". If not follow the instructions here to create a project: https://developers.google.com/workspace/guides/create-project
6. In the google cloud console search for compute engine API, and click the blue Enable button (beware this may take a long time). Do not worry about creating credentials for the API.


### Setup SSH keys
In order to connect to GCP you will need to register an SSH key. Install ssh if you do not already have it (on Ubuntu this is `sudo apt-get install ssh`)

If you do not already have an ssh key-pair, run the following command locally to generate ssh keys.
`ssh-keygen -t rsa -f ~/.ssh/KEY_FILENAME -C USERNAME -b 2048`

To add a public SSH key to the project metadata using the Google Cloud console, do the following:

1. In the Google Cloud console, go to the Metadata page.

2. Click the SSH keys tab.

3. Click Add SSH Key.

4. In the SSH key field that opens, add the public SSH key you generated earlier. The key must be in one of the following formats:
`KEY_VALUE USERNAME`. Replace the following:
-KEY_VALUE: the public SSH key value
-USERNAME: your username. For example, cloudysanfrancisco or cloudysanfrancisco_gmail_com. Note the USERNAME can't be root.

5. Click Save.

6. Remember the Username field (you will need it later for setting up the control machine)

### Setting up Google Virtual Private Cloud (VPC)
Next, you will need to create your own Virtual Private Cloud network. To do so: 

1. In the Google Cloud console, go to the VPC networks page.

2. Click Create VPC network.

3. Enter a Name for the network (we recommend `autobahn-vpc`). The description field is optional.

4. Maximum transmission unit (MTU): Choose 1460 (default)

5. Choose Automatic for the Subnet creation mode.

6. In the Firewall rules section, select the "autobahn-vpc-allow-internal", "autobahn-vpc-allow-ssh", "autobahn-vpc-allow-rdp", "autobahn-vpc-allow-icmp". The rules address common use cases for connectivity to instances.

7. Select Regional for the Dynamic routing mode for the VPC network.

8. Click Create.

Beware that it may take some time for the vpc network to be created.

### Create Instance Templates
We're now ready to create an Instance Template for each region we need, containing the respective hardware configurations we will use.

We used the following four regsions in our experiments: us-east5, us-east1, us-west1, us-west4. 
Create one instance template per region as follows:

1. In the Google Cloud console, go to the Instance templates page.

2. Click Create instance template.

3. Give instance template a name

4. Select the Location as follows: Choose Regional.

5. Select the Region where you want to create your instance template (one of us-east5, us-east1, us-west1, or us-west4).

6. Under Machine configuration select the T2D series (under General purpose category)
7. For Machine type select t2d-standard-16 (16vCPU, 64 GB of memory)
8. Under Availability policies choose Spot for VM provisioning model (to save costs). The default spot VM settings are fine but make sure On VM termination is set to Stop.
9. Scroll down to Boot disk and click Change. For Operating system select Ubuntu. For Version make sure Ubunu 20.04 LTS is selected. For Boot disk type select Balanced persistent disk. This is important! If you use a HDD then writing to disk may become a bottleneck. For size put 20 GB. No need to change anything in advanced configuration.
10. Under Identity and API access select the "Allow full access to all Cloud APIs" option
11. The default Firewall options are fine (unchecked all boxes)
12. Under Network interfaces change the network interface to "autobahn-vpc". Subnetwork is Auto subnet. IP stack type is IPv4. External IPv4 address is Ephemeral. The Network Service Tier is Premium. Don't enable DNS PTR Record.
13. No need to add any additional disks
14. Under Security make sure Turn on vTPM and Turn on Integrity Monitoring is checked. Make sure "Block project-wide SSH keys" is unchecked
15. No need to change anything in the Management or Sole-tenancy sections

Create one last instance template to serve as the control machine. Pick `us-central1` for the region. Name this instance template `autobahn-instance-template` (the scripts assume this is the name of the control machine).
For this instance template select Standard instead of Spot for VM provisioning model (so it won't be pre-empted while running an experiment).
We recommend you pick t2d-standard-4 (instead of t2d-standard-16) for the machine type for the control machine to save costs.

### Setting up Control Machine
1. In Google cloud console go to the VM instances page
2. Select the "Create instance" blue button
3. On the left sidebar select "New VM instance from template"
4. Select `autobahn-instance-template` from the list of templates
5. Change the name field to `autobahn-instance-template`
6. Double check that the rest of the options match what is in `autobahn-instance-template`
7. Wait for the instance to start (you can see the status indicator turn green when that is ready)
8. To connect to this instance from ssh copy the External IP address, and run the following command in the terminal:
`ssh -i SSH_PRIVATE_KEY_LOCATION USERNAME@EXTERNAL_IP_ADDRESS`, where SSH_PRIVATE_KEY_LOCATION is the path of the corresponding ssh private key, USERNAME is the username of the SSH key (found in the Metadata page under SSH keys), and EXTERNAL_IP_ADDRESS
9. We highly recommend you create two folders in the home directory on the control machine for convenience: `autobahn-bullshark` and `hotstuff-baselines`. Navigate to the `autobahn-bullshark` folder, clone the `autobahn-artifact` repo, and checkout `autobahn-simple-sender`. Then navigate to the `hotstuff-baselines` folder, clone the `autobahn-artifact` repo, and checkout the `vanilla-hs-framework` branch. Having this structure will allow you to change parameters and run experiments for different baselines much faster than checking out different branches each time.
10. Follow the Install Dependencies section on the control machine
11. Follow the Generate SSH Keys section on the control machine to generate a new SSH keypair on the control machine and add it to the metadata console

## Running Experiments

i.e. what scripts to run, what configs to give, and how to collect/interpret results.
-> fab remote
Now that you have setup GCP, you are ready to run experiments on GCP!
Follow the GCP Config instructions for both `autobahn-bullshark` and `hotstuff-baselines` folders.

### GCP Config
The GCP config is found in `autobahn-artifact/benchmark/settings.json`. You will need to change the following:
1. `key`: change the `name` (name of the private SSH key) and `path` fields to match the key you generated in the prior section
The `port` field will remain the same (value of 5000).
2. `repo`: The `name` field will remain the same (value of autobahn-artifact). You will need to change the `url` field to be the url of the artifact github repo. Specifically, you will need to prepend your personal access token to the beginning of the url. The url should be in this format: "https://TOKEN@github.com/neilgiri/autobahn-artifact", where `TOKEN` is the name of your personal access token. `branch` specifies which branch will be run on all the machines. This will determine which system ends up running. Only select an Autobahn or Bullshark branch if you are under the `autobahn-bullshark` folder. Similarly, only select a Vanilla HotStuff or Batched HotStuff branch if you are under the `hotstuff-baselines` folder.
3. `project_id`: the project id is found by clicking the the dropdown of "My First Project" on the top left side, and looking at the ID field.
4. `instances`: `type` (value of t2d-standard-16) and `regions` (value of ["us-east1-b", "us-east5-a", "us-west1-b", "us-west4-a"])will remain the same. If you select different regions then you will need to change the regions field to be the regions you are running in. You will need to change `templates` to be the names of the instance templates you created. The order matters, as they should correspond to the order of each region. The path should be in the format "projects/PROJECT_ID/regions/REGION_ID/instanceTemplates/TEMPLATE_ID", where PROJECT_ID is the id of the project you created in the prior section, REGION_ID is the name of the region without the subzone (i.e. us-east1 NOT us-east1-a).

### GCP Benchmark commands
1. If you want to run an Autobahn or Bullshark experiment navigate to `autobahn-bullshark/autobahn-artifact/benchmark`. If you want to run a Vanilla HotStuff or a Batched HotStuff experiment navigate to `hotstuff-baselines/autobahn-artifact/benchmark`.
2. For the first experiment, run `fab create` which will create machines based off your instance templates. For subsequent experiments, you will not need to run `fab create` as the instances will already have been created. Anytime you delete the VM instances you will need to run `fab create` to recreate them.
3. Then run `fab install` which will install rust and the dependencies on these machines. Like `fab create` you only need to run this command one time after the creation of the VMs.
4. Finally `fab remote` will launch a remote experiment with the parameters specified in `fabfile.py`. The next section will show you what each parameter controls. The `fab remote` command should show a progress bar of how far along it is until completion. Note that the first time running the command may take a long time but subsequent trials should be faster.

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


They specify the number of primaries (nodes) and workers per primary (workers) to deploy, the input rate (tx/s) at which the clients submits transactions to the system (rate), the size of each transaction in bytes (tx_size), the number of faulty nodes ('faults), and the duration of the benchmark in seconds (duration). 
The minimum transaction size is 9 bytes, this ensure that the transactions of a client are all different. 
The benchmarking script will deploy as many clients as workers and divide the input rate equally amongst each client. 
For instance, if you configure the testbed with 4 nodes, 1 worker per node, and an input rate of 1,000 tx/s (as in the example above), the scripts will deploy 4 clients each submitting transactions to one node at a rate of 250 tx/s. 
When the parameters faults is set to f > 0, the last f nodes and clients are not booted; the system will thus run with n-f nodes (and n-f clients).

### Autobahn Parameters
The nodes parameters differ between each system. We first show the node parameters for Autobahn.

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

## Reading Output Results
The performance numbers are found in the `autobahn-artifact/benchmark/results` folder.

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
