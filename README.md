# Autobahn: Seamless high speed BFT - SOSP24 Artifact 
This is the repository for the Artifact Evaluation of SOSP'24 proceeding: "Autobahn: Seamless high speed BFT".

For all questions about the artifact, including troubleshooting, please e-mail Neil Giridharan <giridhn@berkeley.edu> and Florian Suri-Payer <fsp@cs.cornell.edu>. 


# Table of Contents
1. [Artifact Overview](#artifact)
2. [High Level Claims](#Claims)
3. [Overview of steps to validate Claims](#validating)
4. [Installing Dependencies and Building Binaries](#installing)
5. [Setting up Cloud Environment](#cloud)
6. [Running Experiments](#experiments)


## Artifact Overview <a name="artifact"></a>

This artifact contains, and allows to reproduce, experiments for all figures included in the paper "Autobahn: Seamless high speed BFT". 

It contains a prototype implemententation of Autobahn, as well as the reference implementations used to evaluate baseline systems: VanillaHS, BatchedHS, and Bullshark. Each prototype is located on its *own* branch, named accordingly. For each system, we have provided *two* branches: one containing the base system (e.g. `autobahn`), and another that contains a version modified to simulate blips (e.g. `autobahn-blips`).
Please checkout the corresponding branch when validating claims for a given system and experiment.

Autobahn and all baseline systems are implemented in Rust, using the asynchronous Tokio runtime environment. TCP is used for networking, and ed25519-dalek signatures are used for authentication.
Replicas persist all messages receives to disk, using RocksDB.
Client processes connect to *local* Replica machines and submit dummy payload requests (transactions) only to this replica. Replicas distribute payloads to one another -- the specifics depend on the particular system. 

Orienting oneself in the code: 
For Autobahn and Bullshark, the two main code modules are `worker` and `primary`. The worker layer is responsible for receiving client requests. It forwards data and digests to the primary layer which contains the main consensus logic. The consensus logic is event driven: the core event loop is located in `primary/src/core.rs`, message reception is managed in `primary/src/primary.rs`. 
VanillaHS and BatchedHS consist of main modules `mempool` and `consensus`, which operate analogously: `mempool/src/core.rs` receives and forwards data, and `consensus/src/core.rs` contains the main consensus logic. 

## Concrete claims in the paper
Autobahn is a Byzantine Fault Tolerant (BFT) consensus protocol that aims to hit a sweet spot between high throughput, low latency, and the ability to recover from asynchrony (seamlessness).

- **Main claim 1**: Autobahn matches the Throughput of Bullshark, while reducing latency by a factor of ca. 2x. Autobahn achieves latency comparable to Hotstuff.

- **Main claim 2**: Autobahn avoids protocol-induced hangovers in the presence of blips. This allows Autobahn to reduce hangover times when compared to non-seamless protocols.



## Validating the Claims - Overview <a name="validating"></a>

All our experiments were run using Google Cloud Platform (GCP) (https://console.cloud.google.com/welcome/). To reproduce our results and validate our claims, you will need to 1) instantiate a matching GCP experiment, 2) build the prototype binaries, and 3) run the provided experiment scripts with the (supplied) configs we used to generate our results.

The ReadMe is organized into the following high level sections:

1. *Installing pre-requisites and building binaries*

   To build Autobahn and baseline source code in any of the branches several dependencies must be installed. Refer to section "Installing Dependencies" for detailed instructions on how to install dependencies and compile the code. 

2. *Setting up experiments on GCP* 

     To re-run our experiments, you will need to instantiate a distributed and replicated server (and client) configuration using GCP. 
     <!-- We have provided a public profile as well as public disk images that capture the configurations we used to produce our results. Section "Setting up Cloudlab" covers the necessary steps in detail. Alternatively, you may create a profile of your own and generate disk images from scratch (more work) - refer to section "Setting up Cloudlab" as well for more information. Note, that you will need to use the same Cluster (Utah) and machine types (m510) to reproduce our results. -->


3. *Running experiments*

     To reproduce our results you will need to checkout the respective branch, and run the supplied experiment scripts using the supplied experiment configurations. Section "Running Experiments" includes instructions for using the experiment scripts, modifying the configurations, and parsing the output. 
     

## Installing Dependencies <a name="installing"></a>

### Pre-requisites
We recommend running on Ubuntu 20.04 LTS as this is the environment we have used for our tests. This said, the code should compile and work on most operating systems.

We require several software dependencies. 
- python3, python3-pip
- rust (recommend 1.80 stable)
- clang version <= 14 (for building librocksdb, DO NOT use version 15 or higher)
- tmux

For convenience, we have provided an install script `install_deps.sh` in the `overview` branch that automatically installs the required dependencies.

After installation finishes, navigate to `autobahn-artifact/benchmark` and run `pip install -r requirements.txt`.

#### Manual installation
If not using `install_deps.sh`, but installing the dependencies manually, make sure to:
- update your distribution: `sudo apt-get update`
- pip might already be installed. Remove it, and install it from here: https://bootstrap.pypa.io/get-pip.py. Do not use apt-get. Update the `PATH` environment variable to point to the location of pip.


### Building code: 
Finally, you can build the binaries (you will ned to do this anew on each branch):
Navigate to `autobahn-artifact` directory and build using `cargo build`.
Note: The experiment scripts will automatically build the binaries if they have not been yet. However, we recommend doing it separately to troubleshoot more easily.

## Testing Locally
To quickly confirm that the installation and build succeeded you may run a simple local experiment. 

In order to run a quick test locally:
1. checkout the branch `autobahn` (or checkout the appropriate branch for the system of your choice)
2. navigate to `autobahn-artifact/benchmark/`
3. run `fab local`.

This will run a simple local experiment, using the parameters provided in `fabfile.py` (in `def local()`). 
By default, the experiment is 20s long and uses 4 replicas. The output contains statistics for throughput, latency, etc.
Additional instructions can be found in `benchmark/README`.
> [!WARNING]
> The Readme in branches Autobahn and Bullshark also contains some instructions to run on AWS. 
> These are inherited from Narwhal/Bullshark and have NOT BEEN TESTED by us. 
> We recommend you use the GCP instructions that we have trialed ourselves.


## Setting up GCP
<!-- TODO: Can we provide alternatives on how to run elsehwere? (if the artifact committee cannot run on GCP) -->
<!-- Detail which machines and configs we used (CPU/SSD...). What geo setup (i.e. where machines are located) -->

> [!NOTE] 
> We strongly recommend running on GCP as our experiment scripts are designed to work with GCP. 
> New users to GCP can get $300 worth of free credit (https://console.cloud.google.com/welcome/new), which should be sufficient to reproduce our core claims. Unfortunately, trial access users *cannot* access the machine type used in our evaluation, and must instead use a weaker machine type (more details below). We have NOT evaluated or systems on these machine types. To accurately reproduce our results, we recommend using the same machine types employed in our experiments, and using the SPOT-market to save costs. Re-running only our key data points, using the spot market, should be relatively cheap -- but make sure to terminate machines as soon as you do not need them.

The Google Cloud console is the gateway for accessing all GCP services. You can search for services using the GCP console searchbar.

To create an account:
1. Select `Try For Free` (blue button)
2. Enter your account information for step 1 and step 2
3. Click `Start Free` (blue button)
4. Optionally complete the survey
5. Creating an account should automatically create a project called "My First Project" (you may pick any name you like). If not follow the instructions here to create a project: https://developers.google.com/workspace/guides/create-project
6. In the google cloud console search for compute engine API, and click the blue Enable button (this may take some time to complete). You do not need to create credentials for the API.

<!-- Most of the time we will use the compute engine service to create and manage VMs but occassionally we will use other services.  -->


### Setup SSH keys
In order to connect to GCP you will need to register an SSH key. 

Install ssh if you do not already have it (on Ubuntu this is `sudo apt-get install ssh`)

If you do not already have an ssh key-pair, run the following command locally to generate ssh keys: `ssh-keygen -t rsa -f ~/.ssh/KEY_FILENAME -C USERNAME -b 2048`

To add a public SSH key to the project metadata using the Google Cloud console, do the following:

1. In the Google Cloud console, go to the Metadata page.

2. Click the SSH keys tab.

3. Click Add SSH Key.

4. In the SSH key field that opens, add the public SSH key you generated earlier. 

> [!NOTE] 
> The key must be of the format: `KEY_VALUE USERNAME`.
> KEY_VALUE := the public SSH key value
> USERNAME := your username. For example, cloudysanfrancisco or cloudysanfrancisco_gmail_com. Note the USERNAME can't be root.

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

8. Click Create. It may take some time for the vpc network to be created.

### Create Instance Templates
We're now ready to create an Instance Template for each region we need, containing the respective hardware configurations we will use. We will require a total of 5 instance templates, one for each of the four regions, and one for a control machine.

We used the following four regsions in our experiments: 
- us-east5
- us-east1
- us-west1
- us-west4. 

Create one instance template per region:

1. In the Google Cloud console, go to the Instance templates page.

2. Click Create instance template.

3. Give instance template a name of your choice

4. Select the Location as follows: Choose Regional.

5. Select the Region where you want to create your instance template (one of us-east5, us-east1, us-west1, or us-west4).

6. Under Machine configuration select the T2D series (under General purpose category)

7. For Machine type select t2d-standard-16 (16vCPU, 64 GB of memory)
> [!NOTE] 
> In our experience, free users will only be able to create t2d-standard-4 instances as the max CPU limit per region is capped to 8, and the total number of CPUs allowed at a given time is 32. Since you will need at least 5 machines (4 regions + one control), no machine will be able to have more than 32/5=6 cores.

8. Under Availability policies choose Spot for VM provisioning model (to save costs). The default spot VM settings are fine but make sure On VM termination is set to Stop.

9. Scroll down to Boot disk and click Change. For Operating system select Ubuntu. For Version make sure Ubunu 20.04 LTS is selected. For Boot disk type select Balanced persistent disk. This is important! If you use a HDD then writing to disk may become a bottleneck. For size put 20 GB. No need to change anything in advanced configuration.

10. Under Identity and API access select the "Allow full access to all Cloud APIs" option

11. The default Firewall options are fine (unchecked all boxes)

12. Under Network interfaces change the network interface to "autobahn-vpc". Subnetwork is Auto subnet. IP stack type is IPv4. External IPv4 address is Ephemeral. The Network Service Tier is Premium. Don't enable DNS PTR Record.

13. No need to add any additional disks

14. Under Security make sure Turn on vTPM and Turn on Integrity Monitoring is checked. Make sure "Block project-wide SSH keys" is unchecked

15. No need to change anything in the Management or Sole-tenancy sections

Finally, create one additional instance template to serve as the control machine. Make the following adjustments:
- Select `us-central1` as the region. 
- Name this instance template `autobahn-instance-template` (the scripts assume this is the name of the control machine).
- For this instance template select Standard instead of Spot for VM provisioning model (so it won't be pre-empted while running an experiment).
- We recommend you pick t2d-standard-4 (instead of t2d-standard-16) for the machine type for the control machine to save costs.

### Setting up Control Machine
We are now ready to set up our experiment controller. Follow these steps to instantiate the controller instance template:

1. In the Google cloud console go to the VM instances page

2. Select "Create instance" (blue button)

3. On the left sidebar select "New VM instance from template"


4. Select `autobahn-instance-template` from the list of templates

5. Change the name field to `autobahn-instance-template`

6. Double check that the rest of the options match the configurations in `autobahn-instance-template`

7. Wait for the instance to start (you can see the status indicator turn green when that is ready)

8. To connect to this instance from ssh copy the External IP address, and run the following command in the terminal:

`ssh -i SSH_PRIVATE_KEY_LOCATION USERNAME@EXTERNAL_IP_ADDRESS`, where SSH_PRIVATE_KEY_LOCATION is the path of the corresponding ssh private key, USERNAME is the username of the SSH key (found in the Metadata page under SSH keys), and EXTERNAL_IP_ADDRESS is the ip address of the control machine.

Next, we must setup the control machine environment:

1. Clone the repository. For convenience, we highly recommend you create two folders in the home directory on the control machine: `autobahn-bullshark` and `hotstuff-baselines`. Navigate to the `autobahn-bullshark` folder, clone the `autobahn-artifact` repo, and checkout branch `autobahn`. Then navigate to the `hotstuff-baselines` folder, clone the `autobahn-artifact` repo, and checkout the `vanilla-hs` branch. Having this structure will allow you to change parameters and run experiments for different baselines much faster than checking out different branches each time as Autobahn and Bullshark (and analogously VanillaHS and BatchedHS) share common parameter structure.

2. Install all required dependencies on the control machine. Follow the Install Dependencies section.

3. Generate new SSH keypairs for the control machine and add them to the metadata console. Follow the Generate SSH Keys section.

## Running Experiments

<!-- i.e. what scripts to run, what configs to give, and how to collect/interpret results.
-> fab remote -->

Now that you have setup GCP, you are ready to run experiments on GCP!
Follow the GCP Config instructions for both the `autobahn-bullshark` and `hotstuff-baselines` folders.

### GCP Config
The GCP config is found in `autobahn-artifact/benchmark/settings.json`. You will need to change the following:
1. `key`: change the `name` (name of the private SSH key) and `path` fields to match the key you generated in the prior section
Leave `port` unchanged (should be `5000`).

2. `repo`: Leave `name` unchanged (should be `autobahn-artifact`). You will need to change the `url` field to be the url of the artifact github repo. Specifically, you will need to prepend your personal access token to the beginning of the url. The url should be in this format: "https://TOKEN@github.com/neilgiri/autobahn-artifact", where `TOKEN` is the name of your personal access token. `branch` specifies which branch will be run on all the machines. This will determine which system ends up running. Only select an Autobahn or Bullshark branch if you are in the `autobahn-bullshark` folder. Similarly, only select a Vanilla HotStuff or Batched HotStuff branch if you are in the `hotstuff-baselines` folder.

3. `project_id`: the project id is found by clicking the the dropdown of your project (e.g. "My First Project") on the top left side, and looking at the ID field.

4. `instances`: `type` (value of t2d-standard-16) and `regions` (value of ["us-east1-b", "us-east5-a", "us-west1-b", "us-west4-a"]) should remain unchanged. If you select different regions then you will need to change the regions field to be the regions you are running in. You will need to change `templates` to be the names of the instance templates you created. The order matters, as they should correspond to the order of each region. The path should be in the format "projects/PROJECT_ID/regions/REGION_ID/instanceTemplates/TEMPLATE_ID", where PROJECT_ID is the id of the project you created in the prior section, REGION_ID is the name of the region without the subzone (i.e. us-east1 NOT us-east1-a).

### GCP Benchmark commands
1. If you want to run an Autobahn or Bullshark experiment navigate to `autobahn-bullshark/autobahn-artifact/benchmark`. If you want to run a Vanilla HotStuff or a Batched HotStuff experiment navigate to `hotstuff-baselines/autobahn-artifact/benchmark`.

2. For the first experiment, run `fab create` which will instantiate machines based off your instance templates. For subsequent experiments, you will not need to run `fab create` as the instances will already have been created. Anytime you delete the VM instances you will need to run `fab create` to recreate them. 
> [!NOTE] 
> Spot machines, although cheaper, are not reliable and may be terminated by GCP at any time. If this happens (perhaps an experiment fails), delete all other running instances and re-run `fab create`.

3. Then run `fab install` which will install rust and the dependencies on these machines. Like `fab create` you only need to run this command one time after the creation of the VMs.

4. Finally `fab remote` will launch a remote experiment with the parameters specified in `fabfile.py`. The next section will explain how to configure the parameters. The `fab remote` command should show a progress bar of how far along it is until completion. Note that the first time running the command may take a long time but subsequent trials should be faster.

## Configuring Parameters
The parameters for the remote experiment are found in `benchmark/fabfile.py`. 
To change the parameters locate the `remote(ctx, debug=True)` task section in `fabfile.py`. This task specifies two types of parameters, the benchmark parameters and the nodes parameters. 

> [!NOTE] 
> To reproduce our experiments you do NOT need to change any parameters. This section serves entirely explanation purposes.
> To reproduce a specific experiment simply copy a config of choice from `experiment_configs` into the `fabfile.py` `remote` task. 

The benchmark parameters look as follows:

```
bench_params = {
    'nodes': 4,        
    'workers': 1,   //only applicable for Autobahn/Bullshark
    'co-locate': True,
    'rate': 50_000, 
    'tx_size': 512,
    'faults': 0,
    'duration': 20,
}
```


They specify the number of primaries (nodes) and workers per primary (workers) to deploy (and whether to co-locate them), the input rate (tx/s) at which the clients submits transactions to the system (rate), the size of each transaction in bytes (tx_size), the number of faulty nodes ('faults), and the duration of the benchmark in seconds (duration). 

The minimum transaction size is 9 bytes, this ensure that the transactions of a client are all different. 

The benchmarking script will deploy as many clients as workers and divide the input rate equally amongst each client. 
For instance, if you configure the testbed with 4 nodes, 1 worker per node, and an input rate of 1,000 tx/s (as in the example above), the scripts will deploy 4 clients each submitting transactions to one node at a rate of 250 tx/s. 

When the parameters faults is set to f > 0, the last f nodes and clients are not booted; the system will thus run with n-f nodes (and n-f clients).

The nodes parameters differ between each system. We show and example node parameters for Autobahn.

### Autobahn/Bullshark Parameters

```
node_params = {
    'timeout_delay': 1_000,  # ms
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
}
```
They are defined as follows.

General Protocol parameters:
- `timeout_delay`: The consensus view change timeout value. 
- `header_size`: The preferred header size (= Car payload). Car proposals in Autobahn (and analogously DAG proposals in Bullshark) do not contain transactions themselves, but propose digests of mini-batches (see Eval section). The primary creates a new header when it has completed its previous Car (or for Bullshark, when it has enough DAG parents) and enough batch digests to reach header_size. Denominated in bytes.
- `max_header_delay`: The maximum delay that the primary waits before readying a new header payload, even if the header did not reach max_header_size. Denominated in ms.
- `gc_depth`: The depth of the garbage collection (Denominated in number of rounds).
- `sync_retry_delay`: The delay after which the synchronizer retries to send sync requests in case there was no reply. Denominated in ms.
- `sync_retry_nodes`: Determine with how many nodes to sync when re-trying to send sync-request. These nodes are picked at random from the committee.
- `batch_size`: The preferred mini-batch size. The workers seal a batch of transactions when it reaches this size. Denominated in bytes.
- `max_batch_delay`: The delay after which the workers seal a batch of transactions, even if max_batch_size is not reached. Denominated in ms.

Autobahn specific params
- `use_optimistic_tips`: Whether to enable Autobahn's optimistic tips optimization. If set to True then non-certified car proposals can be sent to consensus; if False, consensus proposals contain only certified car proposals.
- `use_parallel_proposals`: Whether to allow multiple active consensus instances at a time in parallel
- `k`: The maximum number of consensus instances allowed to be active at any time.
- `use_fast_path`: Whether to enable the 3f+1 fast path for consensus
- `fast_path_timeout`: The timeout for waiting for 3f+1 responses on the consensus fast path
- `use_ride_share`: DEPRECATED: Whether to enable the ride-sharing optimization of piggybacking consensus messages on car messages (see Autobahn supplemental material)
- `car_timeout`: DEPRECATED: Used for ride-sharing.
- `use_fast_sync`: Whether to enable the single-round fast sync optimization. If set to False, Autobahn will use the default recursive sync strategy utilized by DAG protocols
- `use_exponential_timeouts`: Whether to enable timeout doubling upon timeouts firing and triggering a View change

Blip simulation framework:
- `simulate_asynchrony`: Whether to simulate blips
- `asynchrony_type`: The specific type of blip.
- `asynchrony_start`: The start times for each blip event
- `asynchrony_duration`: The duration of each blip event
- `affected_nodes`: How many nodes experience blip behavior
- `egress_penalty`: DEPRECATED: For egress blips how much egress delay is added


### VanillaHS/BatchedHS Parameters

```
node_params = {
    'consensus': {
        'timeout_delay': 1_000,
        'sync_retry_delay': 1_000,
        'max_payload_size': 500,
        'min_block_delay': 0,

        'simulate_asynchrony': True,
        'async_type': [3],
        'asynchrony_start': [10_000],
        'asynchrony_duration': [20_000],
        'affected_nodes': [2],
        'egress_penalty': 50,
        'use_exponential_timeouts': False,
    },
    'mempool': {
        'queue_capacity': 10_000_000,
        'sync_retry_delay': 1_000,
        'max_payload_size': 500_000,
        'min_block_delay': 0
    }
}
```

Consensus parameters:
- `timeout_delay`: The consensus view change timeout value. 
- `sync_retry_delay`: The delay after which the synchronizer (consensus layer) retries to send sync requests in case there was no reply. Denominated in ms.
- `max_payload_size`: Consensus batch size. For BatchedHS, payload = digests of minibatches. For VanillaHS, payload = raw transactions.
- `min_block_delay`: IGNORE. Adds delay to consensus.

Mempool params: (only applies to BatchedHS)
- `queue_capacity`: Maximum buffer size for received mini-batches that have not yet reached agreement. This bounds against Byzantine ddos.
- `sync_retry_delay`: The delay after which the synchronizer (mempool layer) retries to send sync requests in case there was no reply. Denominated in ms.
- `max_payload_size`: Mini-batch size, i.e. transactions (in bytes)
- `min_block_delay`: IGNORE.


## Reading Output Results
When an experiment finishes the logs output files are downloaded to the control machine. The experiment results are found in the `autobahn-artifact/benchmark/results` folder. Result names are auto-generated based on the input parameters. For example, `bench-0-4-1-True-240000-512.txt` (0 faults, 4 nodes, 1 worker per node, True=co-locate primary/workers, load = 240000, txn_size = 512).

There are two types of experiment results. 

For Throughput/Latency (Fig. 5) and Scalability (Fig. 6) experiments the scripts produce simple result summaries containing Throughput and Latency averages.
We distinguish between Consensus latency and End-To-End latency.
End-to-end latency captures the time from a replica first receiving a transaction, until the time a transaction is ready for execution. Consensus Latency, in contrast, captures only the time from when a transaction is included in a car proposal. We never report Consensus Latency in the paper, so you can ignore it for the remainder of the document. The difference between End-to-end and consensus latency captures batching latency, and wait time for the next car to be available.

We measure end-to-end latency from the moment the transaction arrives at the mempool (i.e. the client-local replica receives the transaction), to the moment the request executes.

> [!NOTE] 
> The latency does not include the latency of a reply to the client. 
> This is an artefact from the Narwhal framework, and was not trivially to change so we stuck with it. They did this to align the experimental framework with production deployments (e.g. Aptos/Mysten, or CCF) in which servers do not respond to client requests, but instead clients poll for “state”. Since applications are typically smart contracts, the client does not wait for a result (as would be common for RPCs). For such apps, end-to-end “ends” at the replicas.
> Notably though, extra client-latencies are constant, small, and are a cost shared with all other protocols. It does not depend on the type of protocol. Since clients are co-located with a proposing replica the client-to-mempool latency adds only an in-data center ping latency (ca. 15us), while the reply would incur the tail latency of the f+1th closest replica (i.e. 19/28ms in our setup), or only the ping latency if the local replica is trusted (as is common in blockchains). This overhead is thus arguably negligible compared to the overall consensus latencies.


A throughput latency summary looks as follows: 

```
Example for Autobahn, using 200k tx/s supplied load. 
 + RESULTS:
 Consensus TPS: 199,119 tx/s
 Consensus BPS: 101,948,918 B/s
 Consensus latency: 190 ms

 End-to-end TPS: 199,096 tx/s
 End-to-end BPS: 101,937,133 B/s
 End-to-end latency: 231 ms
```

For Blip experiments (Fig. 7 and Fig. 8) we instead record Latency over Time. An example output excerpt looks as follows:
```
---- start-time  ----- end-time ------ latency (diff)
22.39800000190735,22.634999990463257,0.2369999885559082
22.401000022888184,22.634999990463257,0.23399996757507324
22.424000024795532,23.784000158309937,1.3600001335144043
...
23.424000024795532,24.31000018119812,0.8860001564025879
23.426000118255615,23.782999992370605,0.35699987411499023
23.526000022888184,23.782999992370605,0.2569999694824219
23.54800009727478,23.784000158309937,0.23600006103515625
```
A given row describes the *average* `latency` of transactions that started at `start-time`.

In order to identify a blip start, search for the first row that shows a spike in latency (in this case row 3). Blip starts correspond roughly to the `asynchrony` start specified in the fabfile.py parameters, but may deviate several seconds due to various start-up overheads. 
To identify the end of a hangover, search for rows that return latency to their pre-blip value (in this case row 7). The difference constitutes the `blip + hangover` duration (in this case ca 1.1s). To isolate the hangover duration, subtract from the `blip + hangover` duration the `asynchrony_duration` specified in the fabfile.py parameters. 
> [!NOTE] 
> We note that blip durations are slightly noisy, as 1) latency points are recorded as 1 second averages, and 2) blips may be slightly longer than the specified `asynchrony_duration` depending on the size of the timeout parameter, and the timing of a view change. For instance, when simulating leader failures, a new faulty leader may be elected at the very end of a blip, causing the effective blip duration to be extended by the timeout duration. 

## Reproducing Results
The exact configs (peak points) and results (all data points) for each of our eperiments can be found on branch `overview`, respectively in folders `experiment_configs` and `paper-results`.
To run a specific experiment copy and paste an experiment config into the `fabfile.py` `remote` task. 

For simplicity, we summarize here only the key results necessary to validate our claims. The respective outputs are included in `experiment_configs` for convenience.

### Performance under ideal conditions
All systems were run using `n=4` machines, with one machine located in each region. The configs can be found in `experiment_configs/main-graph/`. 

The reported peak results in Fig. 5 were roughly:
```
      - Autobahn: Throughput: ~234k tx/s, Latency: ~280 ms    
      - Bullshark: Throughput: ~234k tx/s Latency: ~592 ms     
      - BatchedHS: Throughput: ~189k tx/s, Latency: ~333 ms    
      - VanillaHS: Throughput: ~15k tx/s, Latency: ~365 ms       
```
To reproduce all data points, simply adjust the `rate` parameter (input load).

### Scalability
We evaluated all systems using the same setup as above, but for different levels of n: `n=4`, `n=12`, and `n=20`. The results for `n=4` follow from Fig. 5.

To configure the scaling factor, one must modify the `create` task in `fabfile.py`: set `create(ctx, nodes=k)`, where k = n/regions. 
For example, for n=4, set nodes=1; for n=20, use nodes=5.  

We report only the rough peak results for `n=20`. The associated configs can be found in `experiment_configs/scaling-graph/`.
```
      - Autobahn: Throughput: ~227k tx/s, Latency: ~303 ms  (233,000 load)   
      - Bullshark: Throughput: ~227k tx/s Latency: ~631 ms  (232,500 load)    
      - BatchedHS: Throughput: ~112k tx/s, Latency: ~308 ms  (112,500 load)    
      - VanillaHS: Throughput: ~1.5k tx/s, Latency: ~2002 ms (1,600 load)       
```


### Leader failures
In Fig. 7 we simulate blips caused by leader failures.


We summarize the results for the Blip in Fig.1 / the first blip in Fig. 7.
The configs can be found in `experiment_configs/blips-graph/`. 
> [!NOTE] 
> Fig. 7 normalizes the blip start times to a common start time.

The measured blip and hangover durations were roughly:
```
      - Autobahn:   (`220kload-1-fault-exp-timeout.txt`) 
            Blip duration: 1s
            Blip start: 22.4s
            Hangover end: 23.5s
            -> Hangover ~0.1 (minus 0-1s blip noise, so effectively 0)

          
      - VanillaHS:  (`15kload-1fault-exponential-1stimeout.txt`) 
            Blip duration: 1s  (but HS is subject to Double blip behavior, so effetively 3s blip)
            Blip start: 23.5s
            Hangover end: 31.4s
            -> Hangover ~4.9 (minus 0-1s blip noise)
```
> [!NOTE]
> VannillaHS experiences some noisy latency at the beginning due to nodes not booting at the same time, ignore this


### Partition
In Fig. 8 we simulate a blip caused by a temporary, partial partition in which regions us-west and us-east are cut off from one another. 

The configs can be found in `experiment_configs/partition_graph/`
> [!NOTE] 
> Fig. 8 normalizes the blip start times to a common start time.

The measured blip and hangover durations were roughly:
> [!NOTE] 
> The latency numbers reported for Autobahn for Fig. 8 in the submission were slightly higher than expected due to some configuration mistakes. We've fixed this for the rebuttal, and include here the numbers from our re-runs (for both Autobahn and Bullshark).
```
      - Autobahn: (ab_simple_sender_250bs_opt_tips_k4.txt) 
            Blip duration: 20s
            Blip start: 7.9s
            Hangover end: 29.6s
            -> Hangover ~1.7s (minus 0-1s blip noise)

      - Bullshark: (bullshark_250bs_opt_tips_k4.txt) REPLACE
            start: 7.1   end 36.2 -> 29.1 blip - 28. => 8 sec hangover
            Blip duration: 20s
            Blip start: 7.1s
            Hangover end: 36.2s
            -> Hangover ~9.1s (minus 0-1s blip noise)

      - BatchedHS: (batchedhs-partition-500batch-15kload.txt) REPLACE
            Blip duration: 20s
            Blip start: 5.5s
            Hangover end: 35s
            -> Hangover ~9.5s (minus 0-1s blip noise)

      - VanillaHS: (2node-partition-20s-15kload.txt) REPLACE
            Blip duration: 20s
            Blip start: 8.6s
            Hangover end: 49s
            -> Hangover ~20.4s (minus 0-1s blip noise)
```


