use halo2_base::{
    AssignedValue,
    QuantumCell,
    utils::PrimeField, 
    gates::{
        GateInstructions,
        range::{RangeConfig, RangeStrategy}
    },
    halo2_proofs::{
        plonk::{Circuit, ConstraintSystem, Error, Column, Instance}, 
        circuit::{SimpleFloorPlanner, Layouter, Value, Cell, Region},
        halo2curves::{bn256::Fr},
        dev::MockProver
    }, 
    SKIP_FIRST_PASS
};
use halo2_dynamic_sha256::Sha256DynamicConfig;
use halo2_rsa::{
    RSAConfig,
    RSAPubE,
    RSAPublicKey, 
    RSASignature,
    RSAInstructions,
    RSASignatureVerifier,
    BigUintConfig,
    big_uint::decompose_biguint,
    BigUintInstructions
};
use num_bigint::BigUint;
use std::str::FromStr;
use sha2::{Digest, Sha256};
use rsa::{Hash, PaddingScheme, PublicKey, PublicKeyParts, RsaPrivateKey, RsaPublicKey};
use std::fs::File;
use std::io::Read;
use x509_parser::parse_x509_certificate;

struct CertificateVerificationCircuit<F: PrimeField> {
    n_big: BigUint,
    sign_big: BigUint,
    msg: Vec<u8>,
    _f: std::marker::PhantomData<F>,
}

impl<F: PrimeField> CertificateVerificationCircuit<F> {
    const BITS_LEN:usize = 2048;
    const LIMB_BITS:usize = 64;
    const EXP_LIMB_BITS:usize = 5;
    const DEFAULT_E: u128 = 65537;
    const NUM_ADVICE:usize = 50;
    const NUM_FIXED:usize = 1;
    const NUM_LOOKUP_ADVICE:usize = 4;
    const LOOKUP_BITS:usize = 12;

    const MSG_LEN: usize = 1280;
    const SHA256_LOOKUP_BITS: usize = 8;        // is this enough?
    const SHA256_LOOKUP_ADVICE: usize = 8;      // might need to increase this   
}

const DEGREE: usize = 16;


#[derive(Debug,Clone)]
struct CertificateVerificationConfig<F: PrimeField> {
    rsa_config: RSAConfig<F>,
    sha256_config: Sha256DynamicConfig<F>,
    n_instance: Column<Instance>,
    hash_instance: Column<Instance>
}


impl<F: PrimeField> Circuit<F> for CertificateVerificationCircuit<F> {
    type Config = CertificateVerificationConfig<F>;
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        unimplemented!();
    }

    fn configure(meta: &mut ConstraintSystem<F>) -> Self::Config {
        let range_config = RangeConfig::configure(
            meta, RangeStrategy::Vertical, 
            &[Self::NUM_ADVICE], 
            &[Self::NUM_LOOKUP_ADVICE], 
            Self::NUM_FIXED, 
            Self::LOOKUP_BITS, 
            0, 
            DEGREE  // Degree set to 13
        );
        let biguint_config = BigUintConfig::construct(range_config.clone(), Self::LIMB_BITS);
        let rsa_config = RSAConfig::construct(
            biguint_config, 
            Self::BITS_LEN, 
            Self::EXP_LIMB_BITS
        );
        let sha256_config = Sha256DynamicConfig::configure(
            meta, 
            vec![Self::MSG_LEN], 
            range_config, 
            Self::SHA256_LOOKUP_BITS, 
            Self::SHA256_LOOKUP_ADVICE, 
            true
        );
        let n_instance = meta.instance_column();
        let hash_instance = meta.instance_column();
        meta.enable_equality(n_instance);   
        meta.enable_equality(hash_instance);

        Self::Config {
            rsa_config,
            sha256_config,
            n_instance,
            hash_instance
        }
    }

    fn synthesize(&self, config: Self::Config, mut layouter: impl Layouter<F>) -> Result<(), Error> {
        let biguint_config = config.rsa_config.biguint_config();
        config.sha256_config.load(&mut layouter)?;
        biguint_config.range().load_lookup_table(& mut layouter)?;
        let mut first_pass = SKIP_FIRST_PASS;        
        let (public_key_cells, hashed_msg_cells) = layouter.assign_region(
            || "certificat chain verifier", 
            |region| {
                if first_pass {
                    first_pass = false;
                    return Ok((vec![], vec![]));
                }
    
                let mut aux = biguint_config.new_context(region);
                let ctx = &mut aux;
                let e_fix = RSAPubE::Fix(BigUint::from(Self::DEFAULT_E));
                
                // Verify Cert
                let public_key = RSAPublicKey::new(Value::known(self.n_big.clone()), e_fix);     // cloning might be slow
                let public_key = config.rsa_config.assign_public_key(ctx, public_key)?;
    
                let signature = RSASignature::new(Value::known(self.sign_big.clone()));             // cloning might be slow
                let signature = config.rsa_config.assign_signature(ctx, signature)?;
    
                let mut verifier = RSASignatureVerifier::new(
                    config.rsa_config.clone(),
                    config.sha256_config.clone()
                );
    
                let (is_valid, hashed_msg) = verifier.verify_pkcs1v15_signature(ctx, &public_key, &self.msg, &signature)?;
                biguint_config.gate().assert_is_const(ctx, &is_valid, F::one());
                biguint_config.range().finalize(ctx);
                {
                    println!("total advice cells: {}", ctx.total_advice);
                    let const_rows = ctx.total_fixed + 1;
                    println!("maximum rows used by a fixed column: {const_rows}");
                    println!("lookup cells used: {}", ctx.cells_to_lookup.len());
                }                
                let public_key_cells = public_key
                    .n
                    .limbs()
                    .into_iter()
                    .map(|v| v.cell())
                    .collect::<Vec<Cell>>();
                let hashed_msg_cells = hashed_msg
                    .into_iter()
                    .map(|v| v.cell())
                    .collect::<Vec<Cell>>();
                
                Ok((public_key_cells, hashed_msg_cells))
            },
        )?;
        for (i, cell) in public_key_cells.into_iter().enumerate() {
            layouter.constrain_instance(cell, config.n_instance, i)?;
        }
        for (i, cell) in hashed_msg_cells.into_iter().enumerate() {
            layouter.constrain_instance(cell, config.hash_instance, i)?;
        }
        Ok(())

    }

}


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_individual_certificate_verification1() {
        let n_big = BigUint::from_str("25072256773181016646718001929649043437172284752110978827451245341673758518265515824620220750921274366810682997655063764396189586208088373151418554667794497434288488985609147281128841322360351415965542849416626264753334809490058670348969746086550826240466231366937756571002586959049028931911717491153543215680439438869310635579672748993762509151669079269470324005482758681846289579362736138374741810539170313375320254563526825071320753564974728585034652850725343743405185445846611159584133428989207258736588038858395777213215195426888432989580649629247513901205854186589801893897065580902313724004472501504861753897483").unwrap();
        let sign_big = BigUint::from_str("20680993081803492142822962338302702090012972524732502784594581432470613813233541192524722764024920354503190154965692204093222747459365939459424002291455821362931301367726976136689527902609981789422816403441812993066615945663080866662123305222197163642780263995683496902379311723788215176139030235483965841222112599769895713361498037517850307320304159522325294215159771451146358568063624507935867246474696795928738358732497200607481490371297698548140240747180387324746875549163061769304144127629403810621772482130341993919561021750878201129002779865697405289828440208414276640613469910559702285689184938862843121612084").unwrap();
        let mut msg:[u8;1199] =  [48, 130, 4, 171, 160, 3, 2, 1, 2, 2, 16, 72, 169, 57, 255, 16, 50, 77, 117, 218, 86, 91, 206, 228, 145, 213, 244, 48, 13, 6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 11, 5, 0, 48, 129, 183, 49, 11, 48, 9, 6, 3, 85, 4, 6, 19, 2, 85, 83, 49, 22, 48, 20, 6, 3, 85, 4, 10, 19, 13, 69, 110, 116, 114, 117, 115, 116, 44, 32, 73, 110, 99, 46, 49, 40, 48, 38, 6, 3, 85, 4, 11, 19, 31, 83, 101, 101, 32, 119, 119, 119, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 47, 108, 101, 103, 97, 108, 45, 116, 101, 114, 109, 115, 49, 57, 48, 55, 6, 3, 85, 4, 11, 19, 48, 40, 99, 41, 32, 50, 48, 49, 53, 32, 69, 110, 116, 114, 117, 115, 116, 44, 32, 73, 110, 99, 46, 32, 45, 32, 102, 111, 114, 32, 97, 117, 116, 104, 111, 114, 105, 122, 101, 100, 32, 117, 115, 101, 32, 111, 110, 108, 121, 49, 43, 48, 41, 6, 3, 85, 4, 3, 19, 34, 69, 110, 116, 114, 117, 115, 116, 32, 67, 108, 97, 115, 115, 32, 51, 32, 67, 108, 105, 101, 110, 116, 32, 67, 65, 32, 45, 32, 83, 72, 65, 50, 53, 54, 48, 30, 23, 13, 50, 48, 48, 56, 48, 55, 50, 51, 52, 55, 53, 49, 90, 23, 13, 50, 50, 49, 50, 50, 48, 50, 51, 52, 55, 53, 48, 90, 48, 129, 186, 49, 11, 48, 9, 6, 3, 85, 4, 6, 19, 2, 85, 83, 49, 19, 48, 17, 6, 3, 85, 4, 8, 19, 10, 67, 97, 108, 105, 102, 111, 114, 110, 105, 97, 49, 22, 48, 20, 6, 3, 85, 4, 7, 19, 13, 83, 97, 110, 32, 70, 114, 97, 110, 99, 105, 115, 99, 111, 49, 23, 48, 21, 6, 3, 85, 4, 10, 19, 14, 68, 111, 99, 117, 83, 105, 103, 110, 44, 32, 73, 110, 99, 46, 49, 29, 48, 27, 6, 3, 85, 4, 11, 19, 20, 84, 101, 99, 104, 110, 105, 99, 97, 108, 32, 79, 112, 101, 114, 97, 116, 105, 111, 110, 115, 49, 23, 48, 21, 6, 3, 85, 4, 3, 19, 14, 68, 111, 99, 117, 83, 105, 103, 110, 44, 32, 73, 110, 99, 46, 49, 45, 48, 43, 6, 9, 42, 134, 72, 134, 247, 13, 1, 9, 1, 22, 30, 101, 110, 116, 101, 114, 112, 114, 105, 115, 101, 115, 117, 112, 112, 111, 114, 116, 64, 100, 111, 99, 117, 115, 105, 103, 110, 46, 99, 111, 109, 48, 130, 1, 34, 48, 13, 6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 1, 5, 0, 3, 130, 1, 15, 0, 48, 130, 1, 10, 2, 130, 1, 1, 0, 143, 13, 99, 160, 67, 32, 51, 152, 155, 82, 185, 227, 210, 159, 41, 113, 161, 22, 36, 106, 249, 110, 245, 118, 156, 44, 144, 200, 144, 129, 142, 44, 58, 88, 63, 23, 241, 125, 230, 182, 216, 209, 146, 144, 251, 94, 123, 61, 178, 255, 252, 70, 4, 69, 96, 127, 161, 247, 232, 14, 241, 135, 150, 191, 242, 205, 23, 227, 91, 131, 148, 255, 224, 42, 63, 29, 66, 195, 72, 31, 140, 43, 221, 75, 33, 41, 175, 214, 4, 236, 195, 57, 73, 243, 176, 43, 68, 221, 135, 13, 22, 208, 49, 142, 186, 71, 70, 166, 141, 134, 27, 55, 240, 126, 19, 240, 202, 128, 153, 46, 0, 132, 67, 2, 43, 102, 171, 112, 148, 69, 170, 33, 95, 49, 63, 253, 91, 52, 110, 226, 120, 197, 205, 8, 251, 108, 241, 104, 192, 253, 27, 132, 136, 47, 226, 52, 44, 109, 171, 61, 177, 180, 101, 234, 178, 164, 213, 90, 211, 210, 186, 98, 4, 2, 69, 243, 94, 146, 69, 205, 170, 249, 44, 109, 21, 165, 50, 38, 134, 72, 76, 34, 131, 28, 78, 95, 214, 172, 169, 252, 240, 180, 149, 187, 108, 145, 111, 171, 231, 63, 21, 24, 210, 186, 124, 29, 62, 171, 139, 195, 26, 176, 84, 50, 68, 77, 179, 153, 81, 236, 160, 100, 27, 49, 239, 179, 206, 82, 112, 217, 171, 50, 170, 44, 51, 101, 221, 195, 210, 54, 211, 225, 63, 212, 174, 73, 2, 3, 1, 0, 1, 163, 130, 1, 196, 48, 130, 1, 192, 48, 14, 6, 3, 85, 29, 15, 1, 1, 255, 4, 4, 3, 2, 6, 192, 48, 32, 6, 3, 85, 29, 37, 4, 25, 48, 23, 6, 9, 96, 134, 72, 1, 134, 250, 107, 40, 11, 6, 10, 43, 6, 1, 4, 1, 130, 55, 10, 3, 12, 48, 12, 6, 3, 85, 29, 19, 1, 1, 255, 4, 2, 48, 0, 48, 29, 6, 3, 85, 29, 14, 4, 22, 4, 20, 186, 47, 71, 255, 195, 37, 173, 26, 38, 128, 184, 65, 155, 185, 252, 250, 144, 51, 29, 6, 48, 31, 6, 3, 85, 29, 35, 4, 24, 48, 22, 128, 20, 6, 159, 111, 78, 162, 41, 78, 15, 12, 174, 23, 191, 182, 152, 70, 239, 173, 184, 59, 114, 48, 103, 6, 8, 43, 6, 1, 5, 5, 7, 1, 1, 4, 91, 48, 89, 48, 35, 6, 8, 43, 6, 1, 5, 5, 7, 48, 1, 134, 23, 104, 116, 116, 112, 58, 47, 47, 111, 99, 115, 112, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 48, 50, 6, 8, 43, 6, 1, 5, 5, 7, 48, 2, 134, 38, 104, 116, 116, 112, 58, 47, 47, 97, 105, 97, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 47, 99, 108, 97, 115, 115, 51, 45, 50, 48, 52, 56, 46, 99, 101, 114, 48, 55, 6, 3, 85, 29, 31, 4, 48, 48, 46, 48, 44, 160, 42, 160, 40, 134, 38, 104, 116, 116, 112, 58, 47, 47, 99, 114, 108, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 47, 99, 108, 97, 115, 115, 51, 45, 115, 104, 97, 50, 46, 99, 114, 108, 48, 67, 6, 10, 42, 134, 72, 134, 247, 47, 1, 1, 9, 1, 4, 53, 48, 51, 2, 1, 1, 134, 46, 104, 116, 116, 112, 58, 47, 47, 116, 105, 109, 101, 115, 116, 97, 109, 112, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 47, 84, 83, 83, 47, 82, 70, 67, 51, 49, 54, 49, 115, 104, 97, 50, 84, 83, 48, 19, 6, 10, 42, 134, 72, 134, 247, 47, 1, 1, 9, 2, 4, 5, 48, 3, 2, 1, 1, 48, 66, 6, 3, 85, 29, 32, 4, 59, 48, 57, 48, 55, 6, 10, 96, 134, 72, 1, 134, 250, 108, 10, 1, 6, 48, 41, 48, 39, 6, 8, 43, 6, 1, 5, 5, 7, 2, 1, 22, 27, 104, 116, 116, 112, 115, 58, 47, 47, 119, 119, 119, 46, 101, 110, 116, 114, 117, 115, 116, 46, 110, 101, 116, 47, 114, 112, 97];
        
        let circuit = CertificateVerificationCircuit::<Fr> {
            n_big: n_big.clone(),
            sign_big,
            msg: msg.to_vec(),
            _f: std::marker::PhantomData,
        };
        
        let hashed_msg = Sha256::digest(&msg);
        let num_limbs = 2048 / 64;
        let limb_bits = 64;
        let n_fes = decompose_biguint::<Fr>(&n_big, num_limbs, limb_bits);
        
        let hash_fes = hashed_msg.iter().map(|byte| Fr::from(*byte as u64)).collect::<Vec<Fr>>();
        let public_inputs = vec![n_fes,hash_fes];
        
        let k = 16;


        let prover = match MockProver::run(k, &circuit, public_inputs) {
            Ok(prover) => prover,
            Err(e) => panic!("{:?}", e),
        };
        assert_eq!(prover.verify(), Ok(()));
    }
}