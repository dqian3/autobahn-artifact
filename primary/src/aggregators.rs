// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote};
use config::{Committee, Stake};
use crypto::Hash as _;
use crypto::{Digest, PublicKey, Signature};
use std::collections::HashSet;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    weight: Stake,
    pub special_valids: Vec<u8>,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
    first_quorum: bool,
    first_special: bool, 
    valid_weight: Stake,
}

impl VotesAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            special_valids: Vec::new(),
            votes: Vec::new(),
            used: HashSet::new(),
            first_quorum: true,
            first_special: true,
            valid_weight: 0,
        }
    }

    // pub fn count_valids(&mut self) -> u8 {
    //     self.special_valids.iter().sum()
    // }

    pub fn append(
        &mut self,
        vote: Vote,
        committee: &Committee,
        header: &Header,
    ) -> DagResult<(Option<Certificate>, bool, bool)> {
        let author = vote.author;

        // Ensure it is the first time this authority votes.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.special_valids.push(vote.special_valid);
        self.votes.push((author, vote.signature));
        self.weight += committee.stake(&author);
        self.valid_weight += committee.stake(&author) * (vote.special_valid as Stake);

        if self.weight >= committee.quorum_threshold() {
            //self.weight = 0; // Ensures quorum is only reached once.  ==> Removed this: We need the votes_aggregator to keep adding votes.
            let special_ready = self.valid_weight >= committee.quorum_threshold();

            if self.first_quorum || (special_ready && self.first_special) { 
                //Only send quorum once per condition
                self.first_quorum = false;
                if self.first_special { 
                    self.first_special = false
                }

                let cert = Certificate {
                    header: header.clone(),
                    special_valids: self.special_valids.clone(),
                    votes: self.votes.clone(),
                };
                return Ok((Some(cert), self.first_quorum, special_ready));
            }
        }
        Ok((None, false, false))
    }
}

/// Aggregate certificates and check if we reach a quorum.
pub struct CertificatesAggregator {
    weight: Stake,
    certificates: Vec<Digest>,
    used: HashSet<PublicKey>,
}

impl CertificatesAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            certificates: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        certificate: Certificate,
        committee: &Committee,
    ) -> DagResult<Option<Vec<Digest>>> {
        let origin = certificate.origin();

        // Ensure we don't count dummy certs towards parent quorum
        if certificate.votes.is_empty() {
            return Ok(None);
        }

        // Ensure it is the first time this authority votes.
        if !self.used.insert(origin) {
            return Ok(None);
        }

        self.certificates.push(certificate.digest());
        self.weight += committee.stake(&origin);
        if self.weight >= committee.quorum_threshold() {
            self.weight = 0; // Ensures quorum is only reached once.
            return Ok(Some(self.certificates.drain(..).collect()));    //drain empties certificates, but keeps allocated memory (drain consumes values in collection vs into_iter that consumes the whole collection)
        }

        
        Ok(None)
    }
}
