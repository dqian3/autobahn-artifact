// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote, QC};
use config::{Committee, Stake};
use crypto::{PublicKey, Signature, Digest};
use std::collections::HashSet;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    consensus_weight: Stake,
    dissemination_weight: Stake,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl VotesAggregator {
    pub fn new() -> Self {
        Self {
            consensus_weight: 0,
            dissemination_weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        vote: Vote,
        committee: &Committee,
        header: &Header,
    ) -> DagResult<(Option<Certificate>, Option<Certificate>)> {
        let author = vote.author;

        // Ensure it is the first time this authority votes.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));
        
        self.votes.push((author, vote.signature));
        self.dissemination_weight += committee.stake(&author);
        self.consensus_weight += committee.stake(&author);

        if self.dissemination_weight >= committee.validity_threshold() {
            self.dissemination_weight = 0;
            let dissemination_cert: Certificate = Certificate {
                author: header.origin(),
                header_digest: header.digest(),
                height: header.height(),
                votes: self.votes.clone(),
            };
            return Ok((Some(dissemination_cert), None));
        }

        if self.consensus_weight >= committee.quorum_threshold() {
            self.consensus_weight = 0;
            let consensus_cert: Certificate = Certificate { 
                author: header.origin(), 
                header_digest: header.digest(), 
                height: header.height(), 
                votes: self.votes.clone() };
            return Ok((None, Some(consensus_cert)));
        }

        Ok((None, None))
    }
}


/// Aggregate consensus info votes and check if we reach a quorum.
pub struct QCMaker {
    weight: Stake,
    votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl QCMaker {
    pub fn new() -> Self {
        Self {
            weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        author: PublicKey,
        vote: (Digest, Signature),
        committee: &Committee,
    ) -> DagResult<Option<QC>> {
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.votes.push((author, vote.1));
        self.weight += committee.stake(&author);
        if self.weight >= committee.quorum_threshold() {
            // Ensure QC is only made once.
            self.weight = 0; 
            return Ok(Some(QC { id: vote.0, votes: self.votes.clone() }))
        }
        
        Ok(None)
    }
}
