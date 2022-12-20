//use crate::common::committee;
// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult, ConsensusError, ConsensusResult};
//use sailfish::error::{DagError, DagResult, ConsensusError, ConsensusResult};
use crate::primary::{Round, View};
//use crate::config::{Committee};
use config::{Committee, WorkerId, Stake};
use crypto::{Digest, Hash, PublicKey, Signature, SignatureService, SecretKey};
use ed25519_dalek::Digest as _;
use ed25519_dalek::Sha512;
use log::debug;
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::{BTreeMap, BTreeSet, HashSet, HashMap};
use std::convert::TryInto;
use std::fmt;
//use crate::messages_consensus::{QC, TC};

#[cfg(test)]
#[path = "tests/messages_tests.rs"]
pub mod messages_tests;

///////////
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Ticket {
    pub qc: QC,
    pub tc: Option<TC>,
    //The Header that is endorsed by the QC/TC
    pub hash: Digest, 
    pub view: View,
    pub round: Round, 
}

impl Ticket {
    pub async fn new(
        hash: Digest, 
        view: View,
        round: Round, 
        qc: QC,
        tc: Option<TC>,
    ) -> Self {
        let ticket = Self {
            hash,
            view,
            round, 
            qc,
            tc,
        
        };
        ticket
    }
    pub fn genesis(committee: &Committee) -> Self {
        let genesis_qc = QC::genesis(committee);
        Ticket {
            qc: genesis_qc.clone(), 
            tc: None, 
            hash: genesis_qc.hash,
            view: genesis_qc.view,
            round: genesis_qc.view_round,
        }
        
    }
}

impl Hash for Ticket {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
         hasher.update(&self.hash);
         hasher.update(&self.view.to_le_bytes());
         hasher.update(&self.round.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl PartialEq for Ticket {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.view == other.view && self.round == other.round
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
    
    //Consensus parent
    pub ticket: Option<Ticket>, 
    pub prev_view_round: Round, //round that was proposed by the last view.
    pub consensus_parent: Option<Digest>, //Digest of the ticket that was committed in the last view
    

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
        consensus_parent: Option<Digest>,

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
            consensus_parent,
        };
        let id = header.digest();
        let signature = signature_service.request_signature(id.clone()).await;
        Self {
            id,
            signature,
            ..header
        }
    }
    
    //Note: This is essentially equivalent to Header::default() but with an author name. ==> Currently no difference in functionality; can use them interchangeably
            //genesis.digest() == default.digest() because we currently don't compute the digest based off the author (we just use ..Self::default)
    //Purpose: The construct provides easier compatibility for modifications. I.e. if one wants to change genesis Header, genesis QC etc. will adapt automatically
    pub fn genesis(committee: &Committee) -> Self {
        let (name, _) = committee.authorities.iter().next().unwrap();
        Header {
            author: *name,
            //parents: Certificate::genesis(committee).iter().map(|x| x.digest()).collect(), //Note: Can't use these parents, because both parents and current header would be in round 0 => malformed
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

    pub fn cert_lookup(&self) -> Digest {
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
        match &self.consensus_parent {
            Some(parent) => hasher.update(parent),
            None => {},
        }

        hasher.update("cert".as_bytes());

        Digest(hasher.finalize().as_slice()[..].try_into().unwrap())
    }

    pub fn new_from_key(
        author: PublicKey,
        view: View,
        round: Round,
        secret: &SecretKey,
        committee: &Committee,
    ) -> Header {
        let header = Header {
            author,
            round,
            signature: Signature::default(),
            is_special: true,
            view,
            //ticket: Some(Ticket::genesis(committee)),
            consensus_parent: Some(Ticket::genesis(committee).digest()),
           ..Header::default()
        };
        let id = header.digest();
        let signature = Signature::new(&id, secret);
        Self {id, signature, ..header }
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
        match &self.consensus_parent {
            Some(parent) => hasher.update(parent),
            None => {},
        }
        
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl PartialEq for Header {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(
            f,
            "{}: B{}({}, {}, special: {}, view: {}, {:?}, {})",
            self.id,
            self.round,
            self.author,
            self.payload.keys().map(|x| x.size()).sum::<usize>(),
            self.is_special,
            self.view,
            self
            .parents
            .iter()
            .map(|x| format!("{}", x))
            .collect::<Vec<_>>(),
        if self.special_parent.is_some() {self.special_parent.clone().unwrap()} else {Digest::default()}
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
        let mut hasher = Sha512::new();
        // hasher.update(&self.id);
        // hasher.update(self.view.to_le_bytes());
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

impl Vote {
    pub fn new_from_key(id: Digest, round: Round, origin: PublicKey, author: PublicKey, secret: &SecretKey) -> Self {
        let vote = Vote {
            id: id.clone(),
            round,
            origin,
            author,
            signature: Signature::default(),
            view: round,
            special_valid: 1,
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
                    ..Header::genesis(committee)
                    //..Header::default()
                },
                ..Self::default()
            })
            .collect()
    }

    pub fn genesis_cert(committee: &Committee) -> Self {
         Self {
                header : Header::genesis(committee),
                ..Self::default()
        }
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
            //DEBUG
            // println!("verifiable digest: {:?}", &self.verifiable_digest());
            // for (key, sig) in &self.votes {
            //     println!("vote signature: {:?}", sig);
            //     println!("vote author: {:?}", key);
            // }
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

    pub fn verifiable_digest(&self) -> Digest {
        if matching_valids(&self.special_valids) {
            let mut hasher = Sha512::new();
            hasher.update(&self.header.id);
            hasher.update(self.round().to_le_bytes());
            hasher.update(&self.origin());
            hasher.update(&self.special_valids[0].to_le_bytes());
            Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
        }
        else{
            panic!("This verfiable digest branch should never be used");
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

    fn valid_weight(&self, committee: &Committee) -> Stake {
        self.votes
            .iter()
            .enumerate()
            .map(|(i, (author, _))| {
                committee.stake(&author) * (self.special_valids[i] as Stake)
                 })
            .sum()
    }

    pub fn is_special_valid(&self, committee: &Committee) -> bool {
        debug!("Special_valid weight: {}", self.valid_weight(committee));
        self.valid_weight(committee) >= committee.quorum_threshold() 
    
    }
    pub fn is_special_fast(&self, committee: &Committee) -> bool {
       
        self.valid_weight(committee) >= committee.fast_threshold() 

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
            "{}: C{}({}, {}, {:?}, {}, view: {})",
            self.digest(),
            self.round(),
            self.origin(),
            self.header.id,
            self.header
                .parents
                .iter()
                .map(|x| format!("{}", x))
                .collect::<Vec<_>>(),
            if self.header.special_parent.is_some() {self.header.special_parent.clone().unwrap()} else {Digest::default()},
            self.header.view,
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
        if self.qc != QC::genesis(committee) {
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

impl AcceptVote {
    pub fn new_from_key(id: Digest, view: View, round: Round, author: PublicKey, secret: &SecretKey) -> Self {
        let vote = AcceptVote {
            hash: id.clone(),
            view: view,
            view_round: round,
            author,
            signature: Signature::default(),
        };
        let signature = Signature::new(&vote.digest(), &secret);
        Self { signature, ..vote }
    }
}

impl PartialEq for AcceptVote {
    fn eq(&self, other: &Self) -> bool {
        self.digest() == other.digest()
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

    pub origin: PublicKey, //Author that issued Header; Necessary only for FastQC to simulate Certificate. (Alternatively, could remove it from Cert?)
                            //==> vote must contain origin too.
                            //Note: origin field is not part of Digest, as digest is used for SlowQC validation.
}

impl QC {
    pub fn genesis(committee: &Committee) -> Self {
        //QC::default()
        let genesis_header = Header::genesis(committee);
        QC {
            hash: genesis_header.id,
            view: genesis_header.view,
            view_round: genesis_header.round,
            origin: genesis_header.author,
            ..QC::default()
        }
    }
    

    pub fn timeout(&self) -> bool {
        self.hash == Digest::default() && self.view != 0
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {

        //genesis QC always valid
        if Self::genesis(committee) == *self {
            return Ok(());
        }

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

        let verifiable_digest;

        //Check if FastQC -- if so, use Cert verification
        if weight >= committee.fast_threshold() {
            //generate Cert digest:
            let mut hasher = Sha512::new();
            hasher.update(&self.hash);
            hasher.update(self.view_round.to_le_bytes());
            hasher.update(&self.origin);  
            hasher.update(&1u8.to_le_bytes());
            verifiable_digest = Digest(hasher.finalize().as_slice()[..32].try_into().unwrap());
        }
        //If SlowQC -- use QC digest == AcceptVote digest
        else{
            verifiable_digest = self.digest();
        }

        // Check the signatures.
        Signature::verify_batch(&verifiable_digest, &self.votes).map_err(ConsensusError::from)
    }
}

impl Hash for QC {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(&self.hash);
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.view_round.to_le_bytes());
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
    //Note, if we accepted a previous TC's proposal, and it is not committed yet, then that TC'proposal must be the local high_prepare/high_accept.

    //Highest header we have Voted on ==> For FP must recover if f+1 are highest (since fast QC could exist)
    pub high_prepare: Option<Header>,  //TODO: Send full header, or just digest?   //If f+1 vouch for this header, then TC can include just digest; if just 1, and there are no conflicts, then send Header
    pub vote_high_prepare: Option<(Digest, View)>, //TODO: FIXME: Does this need the Round too? Don't think so, as the view is only used to compare against high_accept/high_qc.
    
    //Highest cert we have Accepted (AcceptVote) ==> For SP must recover if 1 is highest (since slow QC could exist)
    pub high_accept: Option<Certificate>, 
    pub vote_high_accept: Option<View>,   //Note: Does not need digest. Since only one cert can exist per view, this is unique

    //Highest qc we have committed ==> Forward to confirm commit for view was successful
    pub high_qc: Option<QC>,  
    pub vote_high_qc: Option<View>, //Note: Does not need digest. Since only one qc can exist per view, this is unique

    
    pub view: View,
    pub author: PublicKey,
    pub signature: Signature,
}

impl Timeout {
    pub async fn new(
        high_prepare: Option<Header>,
        high_accept: Option<Certificate>,
        high_qc: Option<QC>,
        view: View,
        author: PublicKey,
        mut signature_service: SignatureService,
    ) -> Self {
        let mut timeout = Self {
            high_prepare,
            high_accept,
            high_qc,
            view,
            author,
            signature: Signature::default(),
            vote_high_prepare: None,
            vote_high_accept: None,
            vote_high_qc: None,
        };
        // Add the unique identifying information for proposals: TC can thus verify just based on this, without including actual proposal (Header/Cert/QC) for all but the winning proposal
        if let Some(header) = timeout.high_prepare.as_ref() {
            timeout.vote_high_prepare = Some((header.id.clone(), header.view.clone()));
        }
        if let Some(cert) = timeout.high_accept.as_ref() {
            timeout.vote_high_accept = Some(cert.header.view.clone());
        }
        if let Some(qc) = timeout.high_qc.as_ref() {
            timeout.vote_high_qc = Some(qc.view.clone());
        }

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


        //NOTE: When verifying TC, we have purged all vote contents besides the winner --> so this step is skipped. Verification is only necessary for the winning proposal

        //Check the validity of embedded proposals
        if let Some(high_prepare) = &self.high_prepare {
            //Ensure it matches vote
            let (dig, view) = self.vote_high_prepare.as_ref().expect("Invalid Prepare Vote");
            ensure!(
                *dig == high_prepare.id && *view == high_prepare.view, 
                ConsensusError::InvalidTimeout(self.clone())
            );
            high_prepare.verify(committee)?;
        }
        if let Some(high_accept) = &self.high_accept {
            //Ensure it matches vote
            //Ensure it matches vote
            let view = self.vote_high_accept.as_ref().expect("Invalid Accept Vote");
            ensure!(
                *view == high_accept.header.view, 
                ConsensusError::InvalidTimeout(self.clone())
            );
            high_accept.verify(committee)?;
        }
        if let Some(high_qc) = &self.high_qc {
            // Check the embedded QC.
            if *high_qc != QC::genesis(committee) {
                let view = self.vote_high_qc.as_ref().expect("Invalid QC Vote");
                ensure!(
                    *view == high_qc.view, 
                    ConsensusError::InvalidTimeout(self.clone())
                );
                high_qc.verify(committee)?;
            }
        }

        Ok(())
    }
}

impl Hash for Timeout {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.view.to_le_bytes());
        if let Some((hp_dig, hp_view)) = self.vote_high_prepare.as_ref() {
            hasher.update(hp_dig);
            hasher.update(hp_view.to_le_bytes());
        }
        if let Some(ha_view) = self.vote_high_accept {
            //hasher.update(ha_dig);                               
            hasher.update(ha_view.to_le_bytes());
        }
        if let Some(qc_view) = self.vote_high_qc {
            hasher.update(qc_view.to_le_bytes());
        }
       
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}

impl fmt::Debug for Timeout {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "TV({}, {}, {:?})", self.author, self.view, self.high_qc)
    }
}

impl Timeout {
    pub fn new_from_key(high_qc: QC, view: View, author: PublicKey, secret: &SecretKey) -> Self {
        let timeout = Timeout {
            high_prepare: None,
            vote_high_prepare: None,
            high_accept: None, 
            vote_high_accept: None,
            vote_high_qc: Some(high_qc.view.clone()),
            high_qc: Some(high_qc),
            view,
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
   
    //pub hash: Digest,
    pub view: View,
    pub votes: Vec<Timeout>,
    pub winning_proposal: Box<(Option<Header>, Option<Certificate>, Option<QC>)>, //TODO: Make this an any type? ==> cast down to Header/Cert/QC
    pub view_round: Round, //Round of the winning proposal
    //pub votes: Vec<(PublicKey, Signature, View)>,
}

impl PartialEq for TC {
    fn eq(&self, other: &Self) -> bool {
        //self.hash == other.hash && self.view == other.view
        *self.winning_proposal == *other.winning_proposal
    }
}

impl TC {
    pub fn new(committee: &Committee, view: View, votes: Vec<Timeout>) -> Self {
        let tc = TC {
            view,
            votes,
            ..Self::default()
        };
        tc.determine_winning_proposal(committee)
    }

    pub fn genesis(committee: &Committee) -> Self {
        //QC::default()
        let genesis_header = Header::genesis(committee);
        TC {
            //hash: genesis_header.id,
            view: genesis_header.view,
            winning_proposal: Box::new((Some(genesis_header), None, None)),
            //view_round: genesis_header.round,
            ..TC::default()
        }
    }

    pub fn determine_winning_proposal(mut self, committee: &Committee) -> Self { //returns the header_digest to be used for TC proposal (I.e. ticket proposal)
        
        //1) keep track of highest prepare
        let mut high_prepare = &Header::default();
        let mut high_prepares: HashMap<Digest, u32> = HashMap::with_capacity(committee.size()); //count appearances, need f+1
        //2) keep track of highest accept
        let mut high_accept = &Certificate::default();
        //3) keep track of highest qc
        let mut high_qc = &QC::default();

        for timeout in &self.votes {
            if let Some(header) = &timeout.high_prepare {
                *high_prepares.entry(header.id.to_owned()).or_default() += 1;
                if high_prepares.get(&header.id).unwrap() == &committee.validity_threshold() && header.view >= high_prepare.view {
                    high_prepare = header;
                }
            } 
            if let Some(cert) = &timeout.high_accept {
                if cert.header.view >= high_accept.header.view {
                    high_accept = cert;
                }
            }
            if let Some(qc) = &timeout.high_qc {
                if qc.view >= high_qc.view {
                    high_qc = qc;
                }
            }
        }
        
        // pick the highest proposal. Break ties by qc > cert > header
        let mut header: Option<Header> = None;
        let mut cert: Option<Certificate> = None;
        let mut qc: Option<QC> = None;
        //let max_cert_qc_view;

        let mut winner: u32 = 2; //2 = qc wins, 1 cert wins, 0 header wins

        if high_accept.header.view > high_qc.view {
            winner = 1;
        }
        if high_prepare.view > max(high_accept.header.view, high_qc.view) {
            winner = 0;
        }

        if winner == 0 {
            header = Some(high_prepare.clone());
            self.view_round = high_prepare.round;
        }
        if winner == 1 {
            cert = Some(high_accept.clone());
            self.view_round = high_accept.header.round;
        }
        if winner == 2 {
            qc = Some(high_qc.clone());
            self.view_round = high_qc.view_round;
        }

        // This version is cleaner, but incurs one copy more.
        // if high_accept.header.view > high_qc.view {
        //     max_cert_qc_view = high_accept.header.view;
        //     cert = Some(high_accept.clone());

        // }
        // else{
        //     max_cert_qc_view = high_qc.view;
        //     qc = Some(high_qc.clone());
        // }
        // if high_prepare.view > max_cert_qc_view {
        //     header = Some(high_prepare.clone());
        //     cert = None;
        //     qc = None;
        // }
        //Note: 
        // If the header is from a higher view than high_accept, and high_accept did commit, then header must extend it. If header does not extend it, then high_accept did not commit so we can ignore.
        self.set_winning_proposal(header, cert, qc)
    }

    pub fn set_winning_proposal(mut self, header: Option<Header>, cert: Option<Certificate>, qc: Option<QC>) -> Self{
        
        //Clear the contents of all timeouts 
        //TC only needs to include votes + winning proposal 
        self.votes = self.votes.into_iter().map(|mut timeout| {
            timeout.high_prepare = None;
            timeout.high_accept = None;
            timeout.high_qc = None;
            timeout
        }).collect();

        //set winning proposal
        self.winning_proposal = Box::new((header, cert, qc));
        
        self
    }

    pub fn validate_winning_proposal(&self, committee: &Committee) -> ConsensusResult<()> {
       
        // count votes to confirm winner.

        //1) keep track of highest prepare
        let mut high_prepare_meta: (&Digest, View) = (&Digest::default(), 0);
        let mut high_prepares: HashMap<Digest, u32> = HashMap::with_capacity(committee.size()); //count appearances, need f+1
        //2) keep track of highest accept
        let mut high_accept_view: View = 0;
        //3) keep track of highest qc
        let mut high_qc_view: View = 0;

        for timeout in &self.votes {
            //Note: If there is multiple headers in the same view with f+1 tie break arbitrarily, but deterministically -- using the order of timeouts. 
            // In theory, 2 headers with f+1 implies no FP happened, so we can even discard the high_prepare. But using it is maximally efficient for progress.
            if let Some((header_dig, header_view)) = &timeout.vote_high_prepare {
                *high_prepares.entry(header_dig.to_owned()).or_default() += 1;
                if high_prepares.get(&header_dig).unwrap() == &committee.validity_threshold() && *header_view >= high_prepare_meta.1 {
                    high_prepare_meta = (header_dig, *header_view);
                }
            } 
            if let Some(cert_view) = timeout.vote_high_accept {
                if cert_view >= high_accept_view {
                    high_accept_view = cert_view;
                }
            }
            if let Some(qc_view) = timeout.vote_high_qc {
                if qc_view >= high_qc_view {
                    high_qc_view = qc_view;
                }
            }
        }
        
        // pick the winning proposal based on the timeout votes. Break ties by qc > cert > header
        let mut winner: u32 = 2; //2 = qc wins, 1 cert wins, 0 header wins

        if high_accept_view > high_qc_view {
            winner = 1;
        }
        if high_prepare_meta.1 > max(high_accept_view, high_qc_view) {
            winner = 0;
        }

        //check that the determinded winning proposal matches the winning proposal included by the TC
        if winner == 0 { //==> High_Prepare won
            let winning_prepare: &Header = self.winning_proposal.0.as_ref().expect("Invalid winning proposal");
            ensure!(
                winning_prepare.id == *high_prepare_meta.0 && winning_prepare.view == high_prepare_meta.1 && self.view_round == winning_prepare.round,
                ConsensusError::InvalidTC(self.clone())
            );
            //verify correctness of winning header
            winning_prepare.verify(committee)?;
            //Ok((Some(winning_prepare.clone()), None, None)) 
        }
        else if winner == 1 { // ==> High_Accept won
            let winning_cert: &Certificate = self.winning_proposal.1.as_ref().expect("Invalid winning proposal");
            ensure!(
                winning_cert.header.view == high_accept_view && self.view_round == winning_cert.header.round,
                ConsensusError::InvalidTC(self.clone())
            );
            //verify correctness of winning cert
            winning_cert.verify(committee)?;
            //Ok((None, Some(winning_cert.clone()), None)) 
        }
        else { //winner == 2 ==> High_QC won
            let winning_qc: &QC = self.winning_proposal.2.as_ref().expect("Invalid winning proposal"); 
            ensure!(
                winning_qc.view == high_qc_view && self.view_round == winning_qc.view_round,
                ConsensusError::InvalidTC(self.clone())
            );
            //verify correctness of winning qc
            winning_qc.verify(committee)?;
            //Ok((None, None, Some(winning_qc.clone()))) 
        }    
        Ok(())
    }

    pub fn verify(&self, committee: &Committee) -> ConsensusResult<()> {

        //genesis TC always valid
        if Self::genesis(committee) == *self {
            return Ok(());
        }

        // Ensure the QC has a quorum.
        let mut weight = 0;
        let mut used = HashSet::new();
        for timeout in self.votes.iter() {
            let name = &timeout.author;
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

        //Verify each vote
        for timeout in &self.votes {
            //timeout.signature.verify(&timeout.digest(), &timeout.author)?; // Check the signatures. (Note: these are only the signatures for the timeout votes, not the signatures for the proposals. We check those in determine/validate winner)
            timeout.verify(committee)?;
        }
        Ok(())
    }

    //Used for debugging: Returns all voted views. 0 by default if no vote was cast for specific type (prepare/accept/qc)
    pub fn high_qc_views(&self) -> Vec<(View, View, View)> {
        self.votes.iter().map(|timeout| {
                                    let mut hp_view = 0;
                                    let mut ha_view = 0;    
                                    let mut hqc_view = 0;

                                    if let Some((_, view)) = timeout.vote_high_prepare.as_ref() {
                                        hp_view = view.clone()
                                    }
                                    if let Some(view) = timeout.vote_high_accept.as_ref() {
                                        ha_view = view.clone()
                                    }
                                    if let Some(view) = timeout.vote_high_qc.as_ref() {
                                        hqc_view = view.clone()
                                    }
                                   
                                    (hp_view, ha_view, hqc_view)
                                })
                            .collect()
    }
}

impl fmt::Debug for TC {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "TC({}, {:?})", self.view, self.high_qc_views())
    }
}


#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Committment {
    //pub commit_round: Round,
    pub commit_view: View
}

impl Hash for Committment {
    fn digest(&self) -> Digest {
        let mut hasher = Sha512::new();
        hasher.update(self.commit_view.to_le_bytes());
        Digest(hasher.finalize().as_slice()[..32].try_into().unwrap())
    }
}
