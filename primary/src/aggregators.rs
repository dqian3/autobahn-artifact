#![allow(dead_code)]
#![allow(unused_variables)]
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult, ConsensusError};
use crate::messages::{Certificate, Header, Vote, QC, Timeout, TC};
use config::{Committee, Stake};
use crypto::{PublicKey, Signature, Digest};
use std::collections::HashSet;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    dissemination_weight: Stake,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
    diss_cert: Option<Certificate>,

    complete: bool, 
}

impl VotesAggregator {
    pub fn new() -> Self {
        Self {
            dissemination_weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
            diss_cert: None,
            complete: false,
        }
    }

    pub fn append(
        &mut self,
        vote: Vote,
        committee: &Committee,
        header: &Header,
    ) -> DagResult<(bool, bool)> {
        if self.complete {
            return Ok((true, false));
        }
        let author = vote.author;
        // Ensure it is the first time this authority votes.
        println!("author is {:?}", author);
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));
       
        self.votes.push((author, vote.signature));
        self.dissemination_weight += committee.stake(&author);

        if self.dissemination_weight >= committee.validity_threshold() {
            //self.dissemination_weight = 0;
            if self.diss_cert.is_none() {
                let dissemination_cert: Certificate = Certificate {
                    author: vote.origin,
                    header_digest: vote.id,
                    height: vote.height,
                    votes: self.votes.clone(),
                };

                self.diss_cert = Some(dissemination_cert);
            }
            self.complete = true;
            //return Ok(self.diss_cert.clone());
            return Ok((true, true));
        }
        Ok((false, false))
        //Ok(self.diss_cert.clone())
    }

    pub fn get(&mut self,) -> DagResult<Option<Certificate>> {
        Ok(self.diss_cert.clone())
    }
}


/// Aggregate consensus info votes and check if we reach a quorum.
pub struct QCMaker {
    weight: Stake,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,

    pub try_fast: bool,  //TODO: Configure it for Fast path (if it's a Quorummaker for Prepare)
    qc_dig: Digest, 
    first: bool, 
}

impl QCMaker {
    pub fn new() -> Self {
        Self {
            weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
            try_fast: false, // explicitly set it. (NOT done via constructor) 
            qc_dig: Digest::default(),
            first: true, 
        }
    }

    pub fn append(
        &mut self,
        author: PublicKey,
        vote: (Digest, Signature),
        committee: &Committee,
    ) -> DagResult<(bool, Option<QC>)> {   //bool = QC is available. Option = Some only if QC ready to be used.
        println!("calling append");
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));
        println!("after ensure");

        self.votes.push((author, vote.1));
        self.weight += committee.stake(&author);
        println!("QC weight is {:?}", self.weight);

        if self.try_fast {
            return self.check_fast_qc(vote.0, committee);
        }
        //else Slow path:
        if self.weight >= committee.quorum_threshold() {
            // Ensure QC is only made once.
            self.weight = 0; 
            return Ok((true, Some(QC { id: vote.0, votes: self.votes.clone() })))
        }
        
        Ok((false, None))
    }

    pub fn check_fast_qc(&mut self, vote_dig: Digest, committee: &Committee) -> DagResult<(bool, Option<QC>)> {
        if self.weight >= committee.fast_threshold() {
            // Ensure QC is only made once.
            self.weight = 0; 
            return Ok((true, Some(QC { id: vote_dig, votes: self.votes.clone() })))
        }
        else if self.weight >= committee.quorum_threshold() {
            self.qc_dig = vote_dig;
            let first = self.first;
            self.first = false;
            return Ok((first, None)); //Only say qc_ready ONCE for 2f+1 => I.e. only one timer will be started
        }

        Ok((false, None))
    }

    //Call this function to fetch slowQC after fastQC timer expires
    pub fn get_qc(&mut self) -> DagResult<(bool, Option<QC>)> {
        ensure!(
            self.qc_dig != Digest::default(),  //I.e. SlowQC is ready!
            DagError::InvalidSlowQCRequest
        );
        return Ok((true, Some(QC { id: self.qc_dig.clone(), votes: self.votes.clone() })));
    }
}

pub struct TCMaker {
    weight: Stake,
    votes: Vec<Timeout>,
    used: HashSet<PublicKey>,
}

impl TCMaker {
    pub fn new() -> Self {
        Self {
            weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    /// Try to append a signature to a (partial) quorum.
    pub fn append(
        &mut self,
        timeout: Timeout,
        committee: &Committee,
    ) -> DagResult<Option<TC>> {
        let author = timeout.author;

        // Ensure it is the first time this authority votes.
        ensure!(
            self.used.insert(author),
            DagError::AuthorityReuse(author)
        );

        let slot = timeout.slot;
        let view = timeout.view;

        // Add the timeout to the accumulator.
        self.votes
            .push(timeout);
        self.weight += committee.stake(&author);
        if self.weight >= committee.quorum_threshold() {
            self.weight = 0; // Ensures TC is only created once.
            return Ok(Some(TC {
                slot,
                view,
                timeouts: self.votes.clone(),
            }));
        }
        Ok(None)
    }
}
