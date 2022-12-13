use crate::Round;
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote};
use config::{Committee, Stake};
use crypto::Hash as _;
use crypto::{Digest, PublicKey, Signature};
use core::panic;
use std::collections::HashSet;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    weight: Stake,
    valid_weight: Stake,
    fast_weight: Stake,
    pub special_valids: Vec<u8>,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
    // first_quorum: bool,
    // first_special: bool, 
}

impl VotesAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            valid_weight: 0,
            fast_weight: 0,
            special_valids: Vec::new(),
            votes: Vec::new(),
            used: HashSet::new(),
            // first_quorum: true,
            // first_special: true,
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
    ) -> DagResult<(Option<Certificate>, bool)> {
        let author = vote.author;


        //println!("Vote aggregator for : header id {}, round {}. Adding vote sent by replica {}", self.round, self.dig.clone(), vote.author.clone());
        // Ensure it is the first time this authority votes.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.special_valids.push(vote.special_valid);
        self.votes.push((author, vote.signature));

        self.weight += committee.stake(&author);
        let vote_valid_weight = committee.stake(&author) * (vote.special_valid as Stake);
        self.valid_weight += vote_valid_weight;
        self.fast_weight += vote_valid_weight;

        let normal_ready = self.weight >= committee.quorum_threshold();
        let special_ready = self.valid_weight >= committee.quorum_threshold();
        let fast_ready = self.fast_weight >= committee.fast_threshold();

        //Currently forming cert (implies broadcast + process) up to 3 times: a) 2f+1 invalid cert, b) 2f+1 valid cert, c) 3f+1 valid cert
            //In the common case, where all nodes are valid, a) and b) happen together.

        //TODO: To avoid redundantly starting both slow and fast path, can make default valid cert gen fast, and unlock slow one after a timer
        //Note: 2f+1 invalid cert should always be able to form asynchronously for DAG progress
                //Note: Even if we put the Dag Quorum behind a timer the DAG would be asynchronous -- albeit not responsive.
                
        if normal_ready || special_ready || fast_ready {
            self.weight = 0; // Ensures normal quorum is only reached once. 
           
            if special_ready {
                self.valid_weight = 0; // Ensures special quorum is only reached once. 
            }
            
            let cert = Certificate {
                    header: header.clone(),
                    special_valids: self.special_valids.clone(),
                    votes: self.votes.clone(),
             };
            return Ok((Some(cert), special_ready));
        }
    
        Ok((None, false))
    }
}

        // if self.weight >= committee.quorum_threshold() {
        //     //self.weight = 0; // Ensures quorum is only reached once.  ==> Removed this: We need the votes_aggregator to keep adding votes.
        //     let special_ready = self.valid_weight >= committee.quorum_threshold();

        //     if self.first_quorum || (special_ready && self.first_special) {
        //         let is_first_quorum = self.first_quorum;
        //         //Only send quorum once per condition
        //         self.first_quorum = false;
        //         if self.first_special { 
        //             self.first_special = false
        //         }

        //         let cert = Certificate {
        //             header: header.clone(),
        //             special_valids: self.special_valids.clone(),
        //             votes: self.votes.clone(),
        //         };
        //         return Ok((Some(cert), is_first_quorum, special_ready));
        //     }
        // }
        // Ok((None, false, false))
//     }
// }

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

        // Ensure we don't count dummy certs towards parent quorum  //NOTE: Parent Quorum won't ever be used anyways since, by def of existance of dummy cert, it must be for an older round
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
