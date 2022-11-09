// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::primary::{Round, View};
use config::{Committee, WorkerId};
use crypto::{Digest, Hash, PublicKey, Signature, SignatureService};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::convert::TryInto;
use std::fmt;
//use sailfish{QC, TC};


///////////
// #[derive(Serialize, Deserialize, Default, Clone)]
// pub struct Ticket {
//     pub qc: Option<QC>,
//     pub tc: Option<TC>,
//     pub round: Round,
// }
// //TODO: Implement Ticket. Need to know references from Consensus, e.g. "use sailfish{QC, TC};"" -- problem: would be cyclic dependency rn. //Should we just 2add our consensus directly into the primary layer; instead of a consensus module?

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Header {
    pub author: PublicKey,
    pub round: Round,
    pub payload: BTreeMap<Digest, WorkerId>,
    pub parents: BTreeSet<Digest>,
    pub id: Digest,
    pub signature: Signature,
    
    pub is_special: bool,
    pub view: View,
    pub round_view: Round, //round that was proposed by the last view.
   // pub ticket: Ticket, //TODO: Add ticket.

}

//NOTE: A header is special if "is_special = true". It contains a view, round_view, and its parents may be just a single edge -- a Digest of its parent header (notably not of a Cert)
// Special headers currently do not need to carry the QC/TC to justify their ticket -- we keep that at the consensu layer. The view and round_view references the relevant QC/TC.
impl Header {
    pub async fn new(
        author: PublicKey,
        round: Round,
        payload: BTreeMap<Digest, WorkerId>,
        parents: BTreeSet<Digest>,
        signature_service: &mut SignatureService,
        is_special: bool,
        view: View,
        round_view: Round,
    ) -> Self {
        let header = Self {
            author,
            round,
            payload,
            parents,
            id: Digest::default(),
            signature: Signature::default(),
            is_special: is_special,
            view: view,
            round_view: round_view,
        };
        let id = header.digest();
        let signature = signature_service.request_signature(id.clone()).await;
        Self {
            id,
            signature,
            ..header
        }
    }

    pub fn verify(&self, committee: &Committee) -> DagResult<()> {
        // Ensure the header id is well formed.
        ensure!(self.digest() == self.id, DagError::InvalidHeaderId);

        // Ensure the authority has voting rights.
        let voting_rights = committee.stake(&self.author);
        ensure!(voting_rights > 0, DagError::UnknownAuthority(self.author));

        // Ensure all worker ids are correct.
        for worker_id in self.payload.values() {
            committee
                .worker(&self.author, &worker_id)
                .map_err(|_| DagError::MalformedHeader(self.id.clone()))?;
        }

        // Check the signature.
        self.signature
            .verify(&self.id, &self.author)
            .map_err(DagError::from)
    }
}

impl Hash for Header {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.author);
        hasher.update(self.round.to_le_bytes());
        for (x, y) in &self.payload {
            hasher.update(x);
            hasher.update(y.to_le_bytes());
        }
        for x in &self.parents {
            hasher.update(x);
        }
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: B{}({}, {})",
            self.id,
            self.round,
            self.author,
            self.payload.keys().map(|x| x.size()).sum::<usize>(),
        )
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "B{}({})", self.round, self.author)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Vote {
    pub id: Digest,  //the header we are voting for.
    pub round: Round,
    pub origin: PublicKey,
    pub author: PublicKey,
    pub signature: Signature,
    pub special_valid: u8,
}

impl Vote {
    pub async fn new(
        header: &Header,
        author: &PublicKey,
        signature_service: &mut SignatureService,
        special_valid: u8, 
    ) -> Self {
        let vote = Self {
            id: header.id.clone(),
            round: header.round,
            origin: header.author,
            author: *author,
            signature: Signature::default(),
            special_valid: 0,
        };
        let signature = signature_service.request_signature(vote.digest()).await;
        Self { signature, ..vote }
    }

    pub fn verify(&self, committee: &Committee) -> DagResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            DagError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature
            .verify(&self.digest(), &self.author)
            .map_err(DagError::from)
    }
}

impl Hash for Vote {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.id);
        hasher.update(self.round.to_le_bytes());
        hasher.update(&self.origin);
        hasher.update(self.special_valid.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Vote {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: V{}({}, {})",
            self.digest(),
            self.round,
            self.author,
            self.id
        )
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Certificate {
    pub header: Header,
    pub special_valids: Vec<u8>,
    pub votes: Vec<(PublicKey, Signature)>,
}

impl Certificate {
    pub fn genesis(committee: &Committee) -> Vec<Self> {
        committee
            .authorities
            .keys()
            .map(|name| Self {
                header: Header {
                    author: *name,
                    ..Header::default()
                },
                ..Self::default()
            })
            .collect()
    }

    pub fn verify(&self, committee: &Committee) -> DagResult<()> {
        // Genesis certificates are always valid.
        if Self::genesis(committee).contains(self) {
            return Ok(());
        }

        // Check the embedded header.
        self.header.verify(committee)?;

        // Ensure the certificate has a quorum.
        let mut weight = 0;
        let mut used = HashSet::new();
        for (name, _) in self.votes.iter() {
            ensure!(!used.contains(name), DagError::AuthorityReuse(*name));
            let voting_rights = committee.stake(name);
            ensure!(voting_rights > 0, DagError::UnknownAuthority(*name));
            used.insert(*name);
            weight += voting_rights;
        }
        ensure!(
            weight >= committee.quorum_threshold(),
            DagError::CertificateRequiresQuorum
        );

        // Check the signatures.

        //If all votes were special_valid or invalid ==> compute single vote digest and verify it (since it is the same for all)
        if(matching_valids(&self.special_valids)){
            Signature::verify_batch(&self.digest(), &self.votes).map_err(DagError::from)
        }
        else{ //compute all the individual vote digests and verify them  (TODO: Since there are only 2 possible types, 0 and 1 ==> Could compute 2 digests, and then insert them in the correct order)
                                                                            //E.g. could re-order Votes to be first all for 0, then all for 1. And call verify_batch separately twice
            let mut digests = Vec::new();
            for (i, (name, sig)) in self.votes.iter().enumerate() {
                
                digests.push({ 
                    let mut hasher = Sha512::new();
                    hasher.update(&self.header.id);
                    hasher.update(self.round().to_le_bytes());
                    hasher.update(&self.origin());
                    hasher.update(self.special_valids[i].to_le_bytes());
                    Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
                }
                )
                //Check special valid.
                //Does one still need to check  QC? Or can one trust cert?  ==> Yes, because only invalid ones need proof => invalid = not forwarded to consensus. For Dag layer makes no difference. 
                // If a byz leader doesn't want to forward to consensus.. thats fine.. same as timing out.
            }
            Signature::verify_batch_multi(&digests, &self.votes).map_err(DagError::from)
        }
       
    }

    pub fn round(&self) -> Round {
        self.header.round
    }

    pub fn origin(&self) -> PublicKey {
        self.header.author
    }

    
}

pub fn matching_valids(vec: &Vec<u8>) -> bool {
    vec.iter().min() == vec.iter().max()
}


impl Hash for Certificate {
    fn digest(&self) -> Digest {
        if(matching_valids(&self.special_valids)){
            let mut hasher = Sha512::new();
            hasher.update(&self.header.id);
            hasher.update(self.round().to_le_bytes());
            hasher.update(&self.origin());
            hasher.update(&self.special_valids[0].to_le_bytes());
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
        }
        else{
            let mut hasher = Sha512::new();
            hasher.update(&self.header.id);
            hasher.update(self.round().to_le_bytes());
            hasher.update(&self.origin());
            hasher.update(self.special_valids.as_ref());
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
        }
    
    }
}

impl fmt::Debug for Certificate {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: C{}({}, {}, {:?})",
            self.digest(),
            self.round(),
            self.origin(),
            self.header.id,
            self.header
                .parents
                .iter()
                .map(|x| format!("{}", x))
                .collect::<Vec<_>>()
        )
    }
}

impl PartialEq for Certificate {
    fn eq(&self, other: &Self) -> bool {
        let mut ret = self.header.id == other.header.id;
        ret &= self.round() == other.round();
        ret &= self.origin() == other.origin();
        ret
    }
}
