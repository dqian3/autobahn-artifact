//use crate::primary::Height;
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, Header, Vote, ConfirmInfo, CertType, ConsensusInfo};
use config::{Committee, Stake};
use crypto::Hash as _;
use crypto::{Digest, PublicKey, Signature};
//use core::panic;
use std::collections::HashSet;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    weight: Stake,
    invalid_weight: Stake,
    pub votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl VotesAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            invalid_weight: 0,
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

        let num_prepare_valids: usize = vote.prepare_special_valids.into_iter().filter(|x| x.1).count();
        let num_confirm_valids: usize = vote.confirm_special_valids.into_iter().filter(|x| x.1).count();

        if num_prepare_valids == header.info_list.len() && num_confirm_valids == header.parent_cert.confirm_info_list.len() {
            self.weight += committee.stake(&author);
        }
        self.invalid_weight += committee.stake(&author);

        let normal: bool = header.info_list.is_empty() && header.parent_cert.confirm_info_list.is_empty() && self.weight >= committee.validity_threshold();
        let special: bool = !(header.info_list.is_empty() && header.parent_cert.confirm_info_list.is_empty()) && self.weight >= committee.quorum_threshold();
        
        let invalidated: bool = self.invalid_weight >= committee.validity_threshold();

        let mut invalid_cert: Option<Certificate> = None;
        let mut valid_cert: Option<Certificate> = None;

        if invalidated {
            self.invalid_weight = 0;
            invalid_cert = Some(Certificate {
                author: header.origin(),
                header_digest: header.digest(),
                height: header.height(),
                votes: self.votes.clone(),
                special_valids: Vec::new(),
                confirm_info_list: Vec::new(),
            });
        }

        if normal || special {
            self.weight = 0;

            let mut info_list: Vec<ConfirmInfo> = Vec::new();
            for info in header.info_list.clone() {
                let consensus_info: ConsensusInfo = info.consensus_info;
                let confirm_info = ConfirmInfo { consensus_info, cert_type: CertType::Prepare };
                info_list.push(confirm_info);
            }

            for info in header.parent_cert.confirm_info_list.clone() {
                let consensus_info: ConsensusInfo = info.consensus_info;
                if info.cert_type == CertType::Prepare {
                    let confirm_info = ConfirmInfo { consensus_info, cert_type: CertType::Commit };
                    info_list.push(confirm_info);
                }
            }

            valid_cert = Some(Certificate {
                author: header.origin(),
                header_digest: header.digest(),
                height: header.height(),
                votes: self.votes.clone(),
                special_valids: Vec::new(),
                confirm_info_list: info_list,
            });
        }

        Ok((valid_cert, invalid_cert))
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
        //let origin = certificate.origin();

        // Ensure we don't count dummy certs towards parent quorum  //NOTE: Parent Quorum won't ever be used anyways since, by def of existance of dummy cert, it must be for an older round
        if certificate.votes.is_empty() {
            return Ok(None);
        }

        // Ensure it is the first time this authority votes.
        /*if !self.used.insert(origin) {
            return Ok(None);
        }

        self.certificates.push(certificate.digest());
        self.weight += committee.stake(&origin);
        if self.weight >= committee.quorum_threshold() {
            self.weight = 0; // Ensures quorum is only reached once.
            return Ok(Some(self.certificates.drain(..).collect()));    //drain empties certificates, but keeps allocated memory (drain consumes values in collection vs into_iter that consumes the whole collection)
        }*/

        
        Ok(None)
    }
}
