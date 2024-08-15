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
TODO: Say in which modules you can find which code roughly. 

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

2. *Setting up experiments on Cloudlab* 

     To re-run our experiments, you will need to instantiate a distributed and replicated server (and client) configuration using GCP. 
     <!-- We have provided a public profile as well as public disk images that capture the configurations we used to produce our results. Section "Setting up Cloudlab" covers the necessary steps in detail. Alternatively, you may create a profile of your own and generate disk images from scratch (more work) - refer to section "Setting up Cloudlab" as well for more information. Note, that you will need to use the same Cluster (Utah) and machine types (m510) to reproduce our results. -->


3. *Running experiments*

     To reproduce our results you will need to checkout the respective branch, and run the supplied experiment scripts using the supplied experiment configurations. Section "Running Experiments" includes instructions for using the experiment scripts, modifying the configurations, and parsing the output. 
     

## Installing Dependencies <a name="installing"></a>
TODO: What hardware req. What software env (ubuntu). What installs (Rust, cargo, tokio, rocks. etc..?)

Building code: 
`cargo build`

## Testing Locally
i.e. quick local run to get some numbers/see that it works (this might already clear the bar for some of the badges)
-> fab local 

## Setting up GCP
TODO: Can we provide alternatives on how to run elsehwere? (if the artifact committee cannot run on GCP)

Detail which machines and configs we used (CPU/SSD...). What geo setup (i.e. where machines are located)

We recommend running on GCP as our experiment scripts are designed to work with GCP. New users to GCP can get $300 worth of free credit, which should be plenty to reproduce our results

### Setup SSH keys
Run the following command locally to generate ssh keys
`ssh-keygen -t rsa -f ~/.ssh/KEY_FILENAME -C USERNAME -b 2048`

To add a public SSH key to project metadata using the Google Cloud console, do the following:

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

### Setup VPC

1. In the Google Cloud console, go to the VPC networks page.

2. Click Create VPC network.

3. Enter a Name for the network (recommend autobahn-vpc).

4. Maximum transmission unit (MTU): Choose 1460 (default)

5. Choose Automatic for the Subnet creation mode.

6. In the Firewall rules section, select zero or more predefined firewall rules. The rules address common use cases for connectivity to instances.

Whether or not you select pre-defined rules, you can create your own firewall rules after you create the network.

Each predefined rule name starts with the name of the VPC network that you are creating, NETWORK. In the IPv4 firewall rules tab, the predefined ingress firewall rule named NETWORK-allow-custom is editable. By default it specifies the source range 10.128.0.0/9, which contains current and future IPv4 ranges for subnets in an auto mode network. The right side of the row that contains the rule, click Edit to select subnets, add additional IPv4 ranges, and specify protocols and ports.

7. Choose the Dynamic routing mode for the VPC network.

8. Click Create.

9. Check that the default 4 firewall rules are there (see `Pre-populated rules in the default network` here https://cloud.google.com/firewall/docs/firewalls)

### Create Instance Templates
The 4 regions we used are: us-east5, us-east1, us-west1, us-west4. Create one instance template per region as follows:

1. In the Google Cloud console, go to the Instance templates page.

2. Click Create instance template.

3. Give instance template a name

4. Select the Location as follows:
Choose Regional.
Select the Region where you want to create your instance template.

5. Select a Machine type.
Choose t2d-standard-16 (16vCPU, 64 GB of memory) (under General purpose category)
Choose Spot for VM provisioning model (to save costs)
Choose 20 GB balanced persistent disk (in the Boot disk section).
Select ubuntu-2004-focal-v20231101 as the Image

6. Click Create to create the template.

Create one last instance template to serve as the control machine. Pick any of the four regions.
For this instance template select Standard instead of Spot for VM provisioning model (so it won't be pre-empted while running an experiment).
We recommend you pick t2d-standard-4 (instead of t2d-standard-16) for the machine type for the control machine to save costs.

## Running Experiment

i.e. what scripts to run, what configs to give, and how to collect/interpret results.
-> fab remote

Explain what parameters to configure in config and what they control. 

Explain what file to look at for results, and which lines/numbers to look for.

## Reproducing Results
Provide ALL configs for each experiment. But suggest they only validate the claims.

Exp 1
- for each system, give our peak numbers + the config. Have them reproduce those

Exp 2
- for each n, and each system give the numbers (i.e. the whole fig as a table)

Exp 3
- show the 3s blip in HS, and lack thereof for us (don't think we need to show the other two blips).
- give the config. Explain how to interpret the data file to see blip duration and hangover duration (Be careful to explain that the numbers can be slightly offset)

Exp 4
- give the configs and run all. Same same.
