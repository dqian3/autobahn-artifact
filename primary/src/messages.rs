// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult, ConsensusError, ConsensusResult};
//use sailfish::error::{DagError, DagResult, ConsensusError, ConsensusResult};
use crate::primary::{Round, View};
//use crate::config::{Committee};
use config::{Committee, WorkerId};
use crypto::{Digest, Hash, PublicKey, Signature, SignatureService, SecretKey};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::convert::TryInto;
use std::fmt;
//use crate::messages_consensus::{QC, TC};

//use crate::error_consensus::{ConsensusError, ConsensusResult};


///////////
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Ticket {
    pub qc: QC,
    pub tc: Option<TC>,
    pub view: View,
}

impl Ticket {
    pub async fn new(
        qc: QC,
        tc: Option<TC>,
        view: View,
    ) -> Self {
        let ticket = Self {
            qc,
            tc,
            view,
        
        };
        ticket
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "T{})",
            self.view,
            // self.qc,
            // self.tc,
        )
    }
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "T{}", self.view)
    }
}


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
    //edges within DAG
    pub special_parent: Option<Digest>, //Digest of the header of the special parent.
    pub special_parent_round: Round, //round of the parent we have special edge to (only used to re-construct parent cert digest in committer)
    pub ticket: Option<Ticket>, 
    //Consensus parent
    pub prev_view_round: Round, //round that was proposed by the last view.
    pub prev_view_header: Option<Digest>, //Digest of the certificate that was committed in the last view
    

}

//NOTE: A header is special if "is_special = true". It contains a view, prev_view_round, and its parents may be just a single edge -- a Digest of its parent header (notably not of a Cert)
// Special headers currently do not need to carry the QC/TC to justify their ticket -- we keep that at the consensu layer. The view and prev_view_round references the relevant QC/TC.
impl Header {
    pub async fn new(
        author: PublicKey,
        round: Round,
        payload: BTreeMap<Digest, WorkerId>,
        parents: BTreeSet<Digest>,
        signature_service: &mut SignatureService,
        is_special: bool,
        view: View,
        special_parent: Option<Digest>,
        special_parent_round: Round,
        ticket: Option<Ticket>,
        prev_view_round: Round,
        prev_view_header: Option<Digest>,

    ) -> Self {
        let header = Self {
            author,
            round,
            payload,
            parents,
            id: Digest::default(),
            signature: Signature::default(),
            is_special,
            view,
            special_parent,
            special_parent_round,
            ticket,
            prev_view_round,
            prev_view_header,
        };
        let id = header.digest();
        let signature = signature_service.request_signature(id.clone()).await;
        Self {
            id,
            signature,
            ..header
        }
    }

    pub fn genesis(committee: &Committee) -> Self {
        let (name, _) = committee.authorities.iter().next().unwrap();
        Header {
            author: *name,
            ..Self::default()
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

    pub fn round(&self) -> Round {
        self.round
    }

    pub fn origin(&self) -> PublicKey {
        self.author
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

        let f: u8 = if self.is_special { 1u8 } else { 0u8 };
        hasher.update(f.to_le_bytes());
        hasher.update(&self.view.to_le_bytes());
        
    
        match &self.special_parent {
            Some(parent) => hasher.update(parent),
            None => {},
        }
        hasher.update(&self.special_parent_round.to_le_bytes());

        hasher.update(&self.prev_view_round.to_le_bytes());
        match &self.prev_view_header {
            Some(parent) => hasher.update(parent),
            None => {},
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
    pub view: View, //FIXME: is this necessary? Given that the header specifies the view. If so, needs to be added to hash.

    //pub is_special: bool, ==> Changed: Just check against "current header" (can confirm id matches) for specialness, view, round view, etc.
    pub special_valid: u8,
    pub qc: Option<QC>,
    pub tc: Option<TC>,
}

impl Vote {
    pub async fn new(
        header: &Header,
        author: &PublicKey,
        signature_service: &mut SignatureService,
        special_valid: u8, 
        qc: Option<QC>, //Note, these do not need to be signed; they are proof themselves.
        tc: Option<TC>,

    ) -> Self {
        let vote = Self {
            id: header.id.clone(),
            round: header.round,
            origin: header.author,
            author: *author,
            signature: Signature::default(),
            view: 1,
            //is_special: header.is_special, 
            special_valid,
            qc,
            tc, 
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

        // if &self.is_special && !&self.special_valid{
        //     match qc {
        //         Some(x) => { }//TODO: Check QC larger than proposed view/prev_view_round. //FIXME: ... must include view... Verify QC sigs }, 
        //         None => { 
        //             match qc {
        //                 Some(x) => {  }, 
        //                 None => { DagError::InvalidSpecialInvalidation}
        //             }
        //         }, 
        //     }
        // } 

        // Check the signature.
        self.signature
            .verify(&self.digest(), &self.author)
            .map_err(DagError::from)
    }
}

impl Hash for Vote {
    fn digest(&self) -> Digest {
        // TODO: This digest function must match the QC digest in order for QCMaker/aggregator to work
        // FIXME: Solve this issue so that there does not need to be a dependency
        let mut hasher = Sha512::new();
        hasher.update(&self.id);
        hasher.update(self.view.to_le_bytes());
        //hasher.update(&self.id);
        //hasher.update(self.round.to_le_bytes());
        //hasher.update(&self.origin);
        //hasher.update(self.is_special);
        //hasher.update(self.special_valid.to_le_bytes());
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

impl Vote {
    pub fn new_from_key(id: Digest, round: Round, author: PublicKey, secret: &SecretKey) -> Self {
        let vote = Vote {
            id: id.clone(),
            round,
            origin: author,
            author,
            signature: Signature::default(),
            view: round,
            special_valid: 0,
            qc: None,
            tc: None,
        };
        let signature = Signature::new(&vote.digest(), &secret);
        Self { signature, ..vote }
    }
}

impl PartialEq for Vote {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
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
        if matching_valids(&self.special_valids) {
            Signature::verify_batch(&self.verifiable_digest(), &self.votes).map_err(DagError::from)
        }
        else{ //compute all the individual vote digests and verify them  (TODO: Since there are only 2 possible types, 0 and 1 ==> Could compute 2 digests, and then insert them in the correct order)
                                                                            //E.g. could re-order Votes to be first all for 0, then all for 1. And call verify_batch separately twice
            let mut digests = Vec::new();
            for (i, _) in self.votes.iter().enumerate() {
                
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

    fn verifiable_digest(&self) -> Digest {
        if matching_valids(&self.special_valids) {
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
            for i in &self.special_valids {
                hasher.update(i.to_le_bytes());
            }
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
        }
    
    }
    
}

pub fn matching_valids(vec: &Vec<u8>) -> bool {
    vec.iter().min() == vec.iter().max()
}


//TODO: FIXME: Currently made it so special_valids is not part of the Cert hash ==> I consider them part of the signature information 
//Double check though if this is fine/safe
impl Hash for Certificate {
    fn digest(&self) -> Digest {
       
            let mut hasher = Sha512::new();
            hasher.update(&self.header.id);
            hasher.update(self.round().to_le_bytes());
            hasher.update(&self.origin());
           
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())

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

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Block {
    pub qc: QC, // QC is equivalent to Commit Certificate in our terminology. Certificate is equivalent to Vote-QC in our terminology
    pub tc: Option<TC>,
    pub author: PublicKey,
    pub view: View,
    pub payload: Vec<Header>, // Change this to be the payload of a header (vector of digests representing mini-batches)
    pub signature: Signature,
}

impl Block {
    pub async fn new(
        qc: QC,
        tc: Option<TC>,
        author: PublicKey,
        view: View,
        payload: Vec<Header>,
        mut signature_service: SignatureService,
    ) -> Self {
        let block = Self {
            qc,
            tc,
            author,
            view,
            payload,
            signature: Signature::default(),
        };
        let signature = signature_service.request_signature(block.digest()).await;
        Self { signature, ..block }
    }

    pub fn genesis() -> Self {
        Block::default()
    }

    pub fn parent(&self) -> &Digest {
        &self.qc.hash
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        let voting_rights = committee.stake(&self.author);
        ensure!(
            voting_rights > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature.verify(&self.digest(), &self.author)?;

        // Check the embedded QC.
        if self.qc != QC::genesis() {
            self.qc.verify(committee)?;
        }

        // Check the TC embedded in the block (if any).
        if let Some(ref tc) = self.tc {
            tc.verify(committee)?;
        }
        Ok(())
    }
}

impl Hash for Block {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.author.0);
        hasher.update(self.view.to_le_bytes());
        for x in &self.payload {
            hasher.update(&x.id);
        }
        hasher.update(&self.qc.hash);
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: HSB({}, {}, {:?}, {})",
            self.digest(),
            self.author,
            self.view,
            self.qc,
            self.payload.len(),
        )
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "HSB{}", self.view)
    }
}

impl PartialEq for Block {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
    }
}

impl Block {
    pub fn new_from_key(
        qc: QC,
        author: PublicKey,
        view: View,
        payload: Vec<Header>,
        secret: &SecretKey,
    ) -> Block {
        let block = Block {
            qc,
            tc: None,
            author,
            view,
            payload,
            signature: Signature::default(),
        };
        let signature = Signature::new(&block.digest(), secret);
        Self { signature, ..block }
    }
}




#[derive(Clone, Serialize, Deserialize)]
pub struct AcceptVote {
    pub hash: Digest,
    pub view: View,
    pub view_round: Round,
    pub author: PublicKey,
    pub signature: Signature,
}

impl AcceptVote {
    pub async fn new(
        header: &Header,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let vote = Self {
            hash: header.id.clone(),
            view: header.view,
            view_round: header.round,
            author,
            signature: Signature::default(),
        };
        let signature = signature_service.request_signature(vote.digest()).await;
        Self { signature, ..vote }
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature.verify(&self.digest(), &self.author)?;
        Ok(())
    }
}

impl Hash for AcceptVote {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.hash);
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.view_round.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for AcceptVote {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "V({}, {}, {})", self.author, self.view, self.hash)
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct QC {
    pub hash: Digest,
    pub view: View,
    pub view_round: Round,
    pub votes: Vec<(PublicKey, Signature)>,
}

impl QC {
    pub fn genesis() -> Self {
        QC::default()
    }

    pub fn timeout(&self) -> bool {
        self.hash == Digest::default() && self.view != 0
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the QC has a quorum.
        let mut weight = 0;
        let mut used = HashSet::new();
        for (name, _) in self.votes.iter() {
            ensure!(!used.contains(name), ConsensusError::AuthorityReuse(*name));
            let voting_rights = committee.stake(name);
            ensure!(voting_rights > 0, ConsensusError::UnknownAuthority(*name));
            used.insert(*name);
            weight += voting_rights;
        }
        ensure!(
            weight >= committee.quorum_threshold(),
            ConsensusError::QCRequiresQuorum
        );
        println!("Made it to qc verify {}, {}", self.digest(), self.votes.len());
        // Check the signatures.
        Signature::verify_batch(&self.digest(), &self.votes).map_err(ConsensusError::from)
    }
}

impl Hash for QC {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.hash);
        //hasher.update(self.view.to_le_bytes());
        //hasher.update(self.prev_view_round.to_le_bytes());
        hasher.update(self.view.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for QC {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "QC({}, {})", self.hash, self.view)
    }
}

impl PartialEq for QC {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.view == other.view
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Timeout {
    pub high_qc: QC,
    pub view: View,
    pub author: PublicKey,
    pub signature: Signature,
}

impl Timeout {
    pub async fn new(
        high_qc: QC,
        view: View,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let timeout = Self {
            high_qc,
            view,
            author,
            signature: Signature::default(),
        };
        let signature = signature_service.request_signature(timeout.digest()).await;
        Self {
            signature,
            ..timeout
        }
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the authority has voting rights.
        ensure!(
            committee.stake(&self.author) > 0,
            ConsensusError::UnknownAuthority(self.author)
        );

        // Check the signature.
        self.signature.verify(&self.digest(), &self.author)?;

        // Check the embedded QC.
        if self.high_qc != QC::genesis() {
            self.high_qc.verify(committee)?;
        }
        Ok(())
    }
}

impl Hash for Timeout {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.high_qc.view.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "TV({}, {}, {:?})", self.author, self.view, self.high_qc)
    }
}

impl Timeout {
    pub fn new_from_key(high_qc: QC, round: Round, author: PublicKey, secret: &SecretKey) -> Self {
        let timeout = Timeout {
            high_qc,
            view: round,
            author,
            signature: Signature::default(),
        };
        let signature = Signature::new(&timeout.digest(), &secret);
        Self {
            signature,
            ..timeout
        }
    }
}

impl PartialEq for Timeout {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
    }
}


#[derive(Clone, Serialize, Deserialize, Default)]
pub struct TC {
    //TODO: TC should contain hash of the header we vote to commit too. This can be an Option, I.e. there might be none for this view. ==> TODO: Need to include quorum of messages to endorse header 
    pub view: View,
    pub votes: Vec<(PublicKey, Signature, View)>,
}

impl TC {
    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {
        // Ensure the QC has a quorum.
        let mut weight = 0;
        let mut used = HashSet::new();
        for (name, _, _) in self.votes.iter() {
            ensure!(!used.contains(name), ConsensusError::AuthorityReuse(*name));
            let voting_rights = committee.stake(name);
            ensure!(voting_rights > 0, ConsensusError::UnknownAuthority(*name));
            used.insert(*name);
            weight += voting_rights;
        }
        ensure!(
            weight >= committee.quorum_threshold(),
            ConsensusError::TCRequiresQuorum
        );

        // Check the signatures.
        for (author, signature, high_qc_view) in &self.votes {
            let mut hasher = Sha512::new();
            hasher.update(self.view.to_le_bytes());
            hasher.update(high_qc_view.to_le_bytes());
            let digest = Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());
            signature.verify(&digest, &author)?;
        }
        Ok(())
    }

    pub fn high_qc_views(&self) -> Vec<View> {
        self.votes.iter().map(|(_, _, r)| r).cloned().collect()
    }
}

impl fmt::Debug for TC {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "TC({}, {:?})", self.view, self.high_qc_views())
    }
}
