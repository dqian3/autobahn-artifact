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

Building code: Cargo build

## Testing Locally
i.e. quick local run to get some numbers/see that it works (this might already clear the bar for some of the badges)
-> fab local 

## Setting up GCP

TODO: Can we provide alternatives on how to run elsehwere? (if the artifact committee cannot run on GCP)

Detail which machines and configs we used (CPU/SSD...). What geo setup (i.e. where machines are located)

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
