use bio::stats::{PHREDProb, LogProb, Prob};
use util::{VarList, Fragment, FragCall, GenomicInterval};
use haplotype_assembly::{generate_flist_buffer, call_hapcut2};
use std::error::Error;
use std::io::prelude::*;
use std::fs::File;
use std::path::Path;
//use std::ops::range;
use rand::{Rng,StdRng,SeedableRng};

static MAX_P_MISCALL_F64: f64 = 0.5;
static MIN_GQ_FOR_PHASING: f64 = 50.0;

fn generate_realigned_pileup(flist: &Vec<Fragment>, n_var: usize) -> Vec<Vec<FragCall>> {
    let mut pileup_lst: Vec<Vec<FragCall>> = vec![vec![]; n_var];

    for fragment in flist {
        for call in fragment.clone().calls {
            if call.qual < LogProb::from(Prob(MAX_P_MISCALL_F64)) {
                pileup_lst[call.var_ix].push(call);
            }
        }
    }
    pileup_lst
}

fn count_alleles(pileup: &Vec<FragCall>) -> (usize, usize) {
    // compute probabilities of data given each genotype
    let mut count0 = 0;
    let mut count1 = 0;

    for call in pileup {
        match call.allele {
            '0' => {
                count0 += 1;
            }
            '1' => {
                count1 += 1;
            }
            _ => {
                panic!("Unexpected allele observed in pileup.");
            }
        }
    }

    (count0, count1)
}

struct HapPost {
    post00: LogProb,
    post01: LogProb,
    post10: LogProb,
    post11: LogProb,
}

fn compute_haplotype_posteriors(pileup: &Vec<FragCall>) -> HapPost {
    // compute probabilities of data given each genotype
    let mut prob00 = LogProb::ln_one();
    let mut prob01 = LogProb::ln_one();
    let mut prob10 = LogProb::ln_one();
    let mut prob11 = LogProb::ln_one();
    //let ln_half = LogProb::from(Prob(0.5));

    for call in pileup {
        let p_call = LogProb::ln_sub_exp(LogProb::ln_one(), call.qual);
        let p_miscall = call.qual;

        match call.allele {
            '0' => {
                prob00 = prob00 + p_call;
                prob01 = prob01 +
                    LogProb::ln_add_exp(call.p_hap1 + p_call, call.p_hap2 + p_miscall);
                prob10 = prob10 +
                    LogProb::ln_add_exp(call.p_hap2 + p_call, call.p_hap1 + p_miscall);
                prob11 = prob11 + p_miscall;
            }
            '1' => {
                prob00 = prob00 + p_miscall;
                prob01 = prob01 +
                    LogProb::ln_add_exp(call.p_hap2 + p_call, call.p_hap1 + p_miscall);
                prob10 = prob10 +
                    LogProb::ln_add_exp(call.p_hap1 + p_call, call.p_hap2 + p_miscall);
                prob11 = prob11 + p_call;
            }
            _ => {
                panic!("Unexpected allele observed in pileup.");
            }
        }
    }

    // these can happen two ways
    prob00 = prob00 + LogProb(2.0f64.ln());
    prob11 = prob11 + LogProb(2.0f64.ln());

    // sum of data probabilities
    let posts: Vec<LogProb> = vec![prob00, prob01, prob10, prob11];
    let total = LogProb::ln_sum_exp(&posts);

    // genotype posterior probabilities
    HapPost {
        post00: prob00 - total,
        post01: prob01 - total,
        post10: prob10 - total,
        post11: prob11 - total,
    }
}


fn compute_posteriors(pileup: &Vec<FragCall>) -> (LogProb, LogProb, LogProb) {
    // compute probabilities of data given each genotype
    let mut prob00 = LogProb::ln_one();
    let mut prob01 = LogProb::ln_one();
    let mut prob11 = LogProb::ln_one();
    let ln_half = LogProb::from(Prob(0.5));

    for call in pileup {
        let p_call = LogProb::ln_sub_exp(LogProb::ln_one(), call.qual);
        let p_miscall = call.qual;

        match call.allele {
            '0' => {
                prob00 = prob00 + p_call;
                prob01 = prob01 + LogProb::ln_add_exp(ln_half + p_call, ln_half + p_miscall);
                prob11 = prob11 + p_miscall;
            }
            '1' => {
                prob00 = prob00 + p_miscall;
                prob01 = prob01 + LogProb::ln_add_exp(ln_half + p_call, ln_half + p_miscall);
                prob11 = prob11 + p_call;
            }
            _ => {
                panic!("Unexpected allele observed in pileup.");
            }
        }
    }

    // sum of data probabilities
    let posts: Vec<LogProb> = vec![prob00, prob01, prob11];
    let total = LogProb::ln_sum_exp(&posts);

    // genotype posterior probabilities
    let post00 = prob00 - total;
    let post01 = prob01 - total;
    let post11 = prob11 - total;

    (post00, post01, post11)
}

pub fn call_haplotypes(flist: &Vec<Fragment>, varlist: &mut VarList) {
    let pileup_lst = generate_realigned_pileup(&flist, varlist.lst.len());

    assert_eq!(pileup_lst.len(), varlist.lst.len());

    let mut vcf_buffer: Vec<Vec<u8>> = Vec::with_capacity(varlist.lst.len());
    let mut phase_variant: Vec<bool> = vec![false; varlist.lst.len()];

    for i in 0..varlist.lst.len() {
        let pileup = &pileup_lst[i];
        let var = &mut varlist.lst[i];


        let (post00, post01, post11): (LogProb, LogProb, LogProb) = compute_posteriors(&pileup);

        let genotype: String;
        let genotype_qual: f64;

        if post00 > post01 && post00 > post11 {
            let p_call_wrong = LogProb::ln_add_exp(post01, post11);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "0/0".to_string();
        } else if post01 > post00 && post01 > post11 {
            let p_call_wrong = LogProb::ln_add_exp(post00, post11);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "0/1".to_string();
        } else {
            let p_call_wrong = LogProb::ln_add_exp(post00, post01);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "1/1".to_string();
        }

        let (count_ref, count_var): (usize, usize) = count_alleles(&pileup);

        var.qual = *PHREDProb::from(post00);
        var.ra = count_ref;
        var.aa = count_var;
        var.genotype = genotype.clone();
        var.gq = genotype_qual;

        if genotype == "0/1" && genotype_qual > MIN_GQ_FOR_PHASING {
            phase_variant[i] = true;
        }

        let line: String = format!("{}\t{}\t.\t{}\t{}\t.\t.\tRA={};AA={}\tGT:GQ\t{}:{}",
                                   var.chrom,
                                   var.pos0 + 1,
                                   var.ref_allele,
                                   var.var_allele,
                                   count_ref,
                                   count_var,
                                   genotype,
                                   genotype_qual);

        let mut vcf_line: Vec<u8> = vec![];
        for u in line.into_bytes() {
            vcf_line.push(u as u8);
        }
        vcf_line.push('\n' as u8);
        vcf_line.push('\0' as u8);
        vcf_buffer.push(vcf_line);
    }
    let frag_buffer = generate_flist_buffer(&flist, &phase_variant);
    let mut hap1: Vec<u8> = vec![0u8; varlist.lst.len()];

    call_hapcut2(&frag_buffer,
                 &vcf_buffer,
                 frag_buffer.len(),
                 vcf_buffer.len(),
                 0.999,
                 &mut hap1);

    for i in 0..hap1.len() {
        match hap1[i] as char {
            '0' => { varlist.lst[i].genotype = "0|1".to_string() }
            '1' => { varlist.lst[i].genotype = "1|0".to_string() }
            _ => {}
        }
    }
}

pub fn call_genotypes_old(flist: &Vec<Fragment>,
                      varlist: &mut VarList,
                      interval: &Option<GenomicInterval>) {
    let pileup_lst = generate_realigned_pileup(&flist, varlist.lst.len());

    assert_eq!(pileup_lst.len(), varlist.lst.len());

    for i in 0..varlist.lst.len() {
        let pileup = &pileup_lst[i];
        let var = &mut varlist.lst[i];

        match interval {
            &Some(ref iv) => {
                if var.chrom != iv.chrom ||
                    var.pos0 < iv.start_pos as usize ||
                    var.pos0 > iv.end_pos as usize {
                    continue;
                }
            }
            &None => {}
        }

        let posts = compute_haplotype_posteriors(&pileup);

        let post00_unphased = posts.post00;
        let post01_unphased = LogProb::ln_add_exp(posts.post01, posts.post10);
        let post11_unphased = posts.post11;

        let genotype: String;
        let genotype_qual: f64;

        if post00_unphased > post01_unphased && post00_unphased > post11_unphased {
            let p_call_wrong = LogProb::ln_add_exp(post01_unphased, post11_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "0/0".to_string();
        } else if post01_unphased > post00_unphased && post01_unphased > post11_unphased {
            let p_call_wrong = LogProb::ln_add_exp(post00_unphased, post11_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);

            // we only show phase if variant was phased in the first round
            // we do consider "flipping", if posteriors shifted when genotyping in round 2.

            genotype = if var.genotype.contains("|") {
                if posts.post01 > posts.post10 { "0|1".to_string() } else { "1|0".to_string() }
            } else { "0/1".to_string() }
        } else {
            let p_call_wrong = LogProb::ln_add_exp(post00_unphased, post01_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "1/1".to_string();
        }

        let (count_ref, count_var): (usize, usize) = count_alleles(&pileup);

        var.qual = *PHREDProb::from(post00_unphased);
        var.ra = count_ref;
        var.aa = count_var;
        var.genotype = genotype;
        var.gq = genotype_qual;
        var.filter = "PASS".to_string();
    }
}



pub fn call_genotypes(flist: &Vec<Fragment>,
                      varlist: &mut VarList,
                      interval: &Option<GenomicInterval>) {

    let n_var = varlist.lst.len();
    let pileup_lst = generate_realigned_pileup(&flist, varlist.lst.len());
    assert_eq!(pileup_lst.len(), varlist.lst.len());

    let max_iterations: usize = 1000000;
    //let sample_every: usize = 1;
    let ln_half = LogProb::from(Prob(0.5));
    let mut rng: StdRng = StdRng::from_seed(&[1]);

    let hap_ixs = vec![0,1];

    // obtain var_frags. var_frags[i] contains a vector with the indices of fragments overlapping
    // the i-th variant
    let mut var_frags: Vec<Vec<usize>> = vec![vec![]; varlist.lst.len()];

    for i in 0..flist.len() {
        for call in &flist[i].calls {
            var_frags[call.var_ix].push(i);
        }
    }

    let mut haps: Vec<Vec<char>> = vec![vec!['0'; n_var]; 2];
    let mut var_phased: Vec<bool> = vec![false; n_var];

    for v in 0..varlist.lst.len() {

        let var = &mut varlist.lst[v];


        if var.genotype == "0|0".to_string() || var.genotype == "0/0".to_string(){
            haps[0][v] = '0';
            haps[1][v] = '0';
        } else if var.genotype == "0|1".to_string(){
            haps[0][v] = '0';
            haps[1][v] = '1';
            var_phased[v] = true;
        } else if var.genotype == "1|0".to_string() {
            haps[0][v] = '1';
            haps[1][v] = '0';
            var_phased[v] = true;
        } else if var.genotype == "1|1".to_string() || var.genotype == "1/1".to_string(){
            haps[0][v] = '1';
            haps[1][v] = '1';
        } else if var.genotype == "0/1".to_string() || var.genotype == "1/0".to_string(){
            if rng.next_f64() < 0.5 {

                haps[0][v] = '0';
                haps[1][v] = '1';

            } else {

                haps[0][v] = '1';
                haps[1][v] = '0';

            }
        } else {
            panic!("Invalid genotype string \"{}\" in Gibbs Sampler genotyping.", var.genotype);
        }
    }

    // p_read_hap[i][j] contains P(R_j | H_i)
    let mut p_read_hap: Vec<Vec<LogProb>> = vec![vec![LogProb::ln_one(); flist.len()]; 2];

    for hap_ix in &hap_ixs {
        for f in 0..flist.len() {
            for call in &flist[f].calls {
                if var_phased[call.var_ix] && call.qual < LogProb::from(Prob(MAX_P_MISCALL_F64)) {
                    // read allele matches haplotype allele
                    if call.allele == haps[*hap_ix][call.var_ix] {
                        p_read_hap[*hap_ix][f] = p_read_hap[*hap_ix][f] + &call.one_minus_qual;
                    } else { // read allele does not match haplotype allele
                        p_read_hap[*hap_ix][f] = p_read_hap[*hap_ix][f] + call.qual;
                    }
                }
            }
        }
    }


    for _ in 0..max_iterations {

        let mut changed = false;

        // take the variants v in random order
        let mut ixvec: Vec<usize> = (0..varlist.lst.len()).collect();
        let ixslice: &mut [usize] = ixvec.as_mut_slice();
        rng.shuffle(ixslice);

        for v_r in ixslice {

            let v = *v_r;

            let var = &mut varlist.lst[v];

            match interval {
                &Some(ref iv) => {
                    if var.chrom != iv.chrom ||
                        var.pos0 < iv.start_pos as usize ||
                        var.pos0 > iv.end_pos as usize {
                        continue;
                    }
                }
                &None => {}
            }

            let mut p_reads: [LogProb; 4] = [LogProb::ln_one();4];

            // let g be the current genotype being considered to switch to
            // then p_read_g[g] contains a vector of tuples (frag_ix, p_read_h0, p_read_h0)
            // that have the probability of each fragment under the new genotypes
            let mut p_read_g: Vec<Vec<(usize, LogProb, LogProb)>> = vec![vec![];4];

            for g in 0..4 {
                for call in &pileup_lst[v] {


                    if !(call.qual < LogProb::from(Prob(MAX_P_MISCALL_F64))) {
                        continue;
                    }

                    // get the value of the read likelihood given each haplotype
                    let (mut p_read_h0, mut p_read_h1) = match call.frag_ix {
                        Some(frag_ix) => (p_read_hap[0][frag_ix], p_read_hap[1][frag_ix]),
                        None => panic!("ERROR: Fragment index is missing in pileup iteration.")
                    };

                    // for each haplotype allele
                    // if that allele on the haplotype changes in g,
                    // then we divide out the old value and multiply in the new value

                    // haplotype 0 at site j was a '1', but now it will be '0'
                    // if call on read was '1', we divide out (1 - q) and multiply in q
                    // if call on read was '0', we divide out q and multiply in (1 - q)

                    if (g == 0 || g == 1) && (haps[0][v] == '1') {
                        if call.allele == '1' {
                            p_read_h0 = p_read_h0 - call.one_minus_qual + call.qual;
                        } else if call.allele == '0' {
                            p_read_h0 = p_read_h0 - call.qual + call.one_minus_qual;
                        } else {
                            panic!("Unsupported allele");
                        }
                    }

                    // haplotype 0 at site j was a '0', but now it will be '1'
                    // if call on read was '0', we divide out (1 - q) and multiply in q
                    // if call on read was '1', we divide out q and multiply in (1 - q)
                    if (g == 2 || g == 3) && (haps[0][v] == '0') {
                        if call.allele == '0' {
                            p_read_h0 = p_read_h0 - call.one_minus_qual + call.qual;
                        } else if call.allele == '1' {
                            p_read_h0 = p_read_h0 - call.qual + call.one_minus_qual;
                        } else {
                            panic!("Unsupported allele");
                        }
                    }

                    // haplotype 1 at site j was a '1', but now it will be '0'
                    // if call on read was '1', we divide out (1 - q) and multiply in q
                    // if call on read was '0', we divide out q and multiply in (1 - q)

                    if (g == 0 || g == 2) && (haps[1][v] == '1') {
                        if call.allele == '1' {
                            p_read_h1 = p_read_h1 - call.one_minus_qual + call.qual;
                        } else if call.allele == '0' {
                            p_read_h1 = p_read_h1 - call.qual + call.one_minus_qual;
                        } else {
                            panic!("Unsupported allele");
                        }
                    }

                    // haplotype 1 at site j was a '0', but now it will be '1'
                    // if call on read was '0', we divide out (1 - q) and multiply in q
                    // if call on read was '1', we divide out q and multiply in (1 - q)
                    if (g == 1 || g == 3) && (haps[1][v] == '0') {
                        if call.allele == '0' {
                            p_read_h1 = p_read_h1 - call.one_minus_qual + call.qual;
                        } else if call.allele == '1' {
                            p_read_h1 = p_read_h1 - call.qual + call.one_minus_qual;
                        } else {
                            panic!("Unsupported allele");
                        }
                    }

                    match call.frag_ix {
                        Some(frag_ix) => {p_read_g[g].push((frag_ix, p_read_h0, p_read_h1));},
                        None => {panic!("Fragment index in pileup call is None.")},
                    }

                    let p_read = LogProb::ln_add_exp(ln_half + p_read_h0, ln_half + p_read_h1);
                    p_reads[g] = p_reads[g] + p_read;
                }
            }

            let p_total = LogProb::ln_sum_exp(&p_reads);

            var.genotype_post[0] = p_reads[0] - p_total;
            var.genotype_post[1] = p_reads[1] - p_total;
            var.genotype_post[2] = p_reads[2] - p_total;
            var.genotype_post[3] = p_reads[3] - p_total;

            let mut max_ix = 5;
            let mut max_val = LogProb::from(Prob(0.0));

            for k in 0..4 {
                if var.genotype_post[k] > max_val {
                    max_ix = k;
                    max_val = var.genotype_post[k];
                }
            }

            match max_ix {
                0 => {if !(haps[0][v] == '0' && haps[1][v] == '0'){changed = true};
                      haps[0][v] = '0';
                      haps[1][v] = '0';},
                1 => {if !(haps[0][v] == '0' && haps[1][v] == '1'){changed = true};
                      haps[0][v] = '0';
                      haps[1][v] = '1';},
                2 => {if !(haps[0][v] == '1' && haps[1][v] == '0'){changed = true};
                      haps[0][v] = '1';
                      haps[1][v] = '0';},
                3 => {if !(haps[0][v] == '1' && haps[1][v] == '1'){changed = true};
                      haps[0][v] = '1';
                      haps[1][v] = '1';},
                _ => {panic!("Invalid genotype  index.")}
            }

            for &(frag_ix, p_read_h0, p_read_h1) in &p_read_g[max_ix] {
                p_read_hap[0][frag_ix] = p_read_h0;
                p_read_hap[1][frag_ix] = p_read_h1;
            }

            let new_qual: f64 = *PHREDProb::from(max_val);

            // the quality score of this variant has increased above the limit
            // we add it from the pool of phased variants
            if new_qual >= MIN_GQ_FOR_PHASING && var_phased[v] == false {

                // need to visit each fragment overlapping vth variant and multiply (add) in the
                // amount the call at this site contributes to fragment likelihoods
                for call in &pileup_lst[v] {
                    if call.qual >= LogProb::from(Prob(MAX_P_MISCALL_F64)) {
                        continue;
                    }

                    let frag_ix = match call.frag_ix {
                        Some(f_ix) => f_ix,
                        None => panic!("ERROR: Fragment index is missing in pileup iteration.")
                    };

                    match call.allele {
                        '0' => {
                            match haps[0][v] {
                                '0' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] + call.one_minus_qual; },
                                '1' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] + call.qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                            match haps[1][v] {
                                '0' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] + call.one_minus_qual; },
                                '1' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] + call.qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                        },
                        '1' => {
                            match haps[0][v] {
                                '0' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] + call.qual; },
                                '1' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] + call.one_minus_qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                            match haps[1][v] {
                                '0' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] + call.qual; },
                                '1' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] + call.one_minus_qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                        },
                        _ => { panic!("Invalid allele in fragment.") }
                    }
                }

                var_phased[v] = true;
            }

            // the quality score of this variant has dipped below the limit
            // we remove it from the pool of phased variants
            if new_qual < MIN_GQ_FOR_PHASING && var_phased[v] == true {

                // need to visit each fragment overlapping vth variant and divide (subtract) out the
                // amount the call at this site contributes to fragment likelihoods

                for call in &pileup_lst[v] {
                    if call.qual >= LogProb::from(Prob(MAX_P_MISCALL_F64)) {
                        continue;
                    }

                    let frag_ix = match call.frag_ix {
                        Some(f_ix) => f_ix,
                        None => panic!("ERROR: Fragment index is missing in pileup iteration.")
                    };

                    match call.allele {
                        '0' => {
                            match haps[0][v] {
                                '0' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] - call.one_minus_qual; },
                                '1' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] - call.qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                            match haps[1][v] {
                                '0' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] - call.one_minus_qual; },
                                '1' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] - call.qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                        },
                        '1' => {
                            match haps[0][v] {
                                '0' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] - call.qual; },
                                '1' => { p_read_hap[0][frag_ix] = p_read_hap[0][frag_ix] - call.one_minus_qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                            match haps[1][v] {
                                '0' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] - call.qual; },
                                '1' => { p_read_hap[1][frag_ix] = p_read_hap[1][frag_ix] - call.one_minus_qual; },
                                _ => { panic!("Invalid allele in haplotype."); }
                            }
                        },
                        _ => { panic!("Invalid allele in fragment.") }
                    }
                }

                var_phased[v] = false;
            }

        }


        // if the haplotypes have not changed in this iteration, then we break
        if !changed {
            break;
        }
    }

    // calculate the fraction of samples containing each genotype state
    // this is our estimate of P(G | R,H)

    for i in 0..varlist.lst.len() {
        let pileup = &pileup_lst[i];
        let var = &mut varlist.lst[i];

        //let total: f64 = var.genotype_counts[0] +  var.genotype_counts[1] +
         //                var.genotype_counts[2] +  var.genotype_counts[3];

        let post00: LogProb = var.genotype_post[0];
        let post01: LogProb = var.genotype_post[1];
        let post10: LogProb = var.genotype_post[2];
        let post11: LogProb = var.genotype_post[3];

        let post00_unphased = post00;
        let post01_unphased = LogProb::ln_add_exp(post01, post10);
        let post11_unphased = post11;

        let genotype: String;
        let mut genotype_qual: f64;

        if post00_unphased > post01_unphased && post00_unphased > post11_unphased {
            let p_call_wrong = LogProb::ln_add_exp(post01_unphased, post11_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "0|0".to_string();
        } else if post01_unphased > post00_unphased && post01_unphased > post11_unphased {
            let p_call_wrong = LogProb::ln_add_exp(post00_unphased, post11_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);

            // we only show phase if variant was phased in the first round
            // we do consider "flipping", if posteriors shifted when genotyping in round 2.

            genotype = if var.genotype.contains("|") {
                if post01 > post10 { "0|1".to_string() } else { "1|0".to_string() }
            } else { "0/1".to_string() }
        } else {
            let p_call_wrong = LogProb::ln_add_exp(post00_unphased, post01_unphased);
            genotype_qual = *PHREDProb::from(p_call_wrong);
            genotype = "1|1".to_string();
        }

        let (count_ref, count_var): (usize, usize) = count_alleles(&pileup);

        var.qual = *PHREDProb::from(post00_unphased);
        var.ra = count_ref;
        var.aa = count_var;
        var.genotype = genotype;
        var.gq = genotype_qual;
        var.filter = "PASS".to_string();

    }
}

pub fn var_filter(varlist: &mut VarList, density_qual: f64, density_dist: usize, density_count: usize, max_depth: Option<u32>) {
    for i in 0..varlist.lst.len() {
        if varlist.lst[i].qual < density_qual { continue; }

        let mut count = 0;
        for j in i + 1..varlist.lst.len() {
            if varlist.lst[j].pos0 - varlist.lst[i].pos0 > density_dist {
                break;
            }
            if varlist.lst[j].qual < density_qual { continue; }
            count += 1;
            if count > density_count {
                for k in i..j + 1 {
                    varlist.lst[k].filter = "dn".to_string();
                }
            }
        }
    }
    match max_depth {
        Some(dp) => {
            for i in 0..varlist.lst.len() {
                if varlist.lst[i].dp > dp as usize {
                    if varlist.lst[i].filter == ".".to_string() || varlist.lst[i].filter == "PASS".to_string() {
                        varlist.lst[i].filter = "dp".to_string();
                    } else {
                        varlist.lst[i].filter.push_str(";dp");
                    }
                }
            }
        }
        None => {}
    }
}

pub fn print_vcf(varlist: &VarList, interval: &Option<GenomicInterval>, output_vcf_file: String) {
    let vcf_path = Path::new(&output_vcf_file);
    let vcf_display = vcf_path.display();
    // Open a file in write-only mode, returns `io::Result<File>`
    let mut file = match File::create(&vcf_path) {
        Err(why) => panic!("couldn't create {}: {}", vcf_display, why.description()),
        Ok(file) => file,
    };

    let headerstr = "##fileformat=VCFv4.2
##source=ReaperV0.1
##FORMAT=<ID=GQ,Number=1,Type=Integer,Description=\"Genotype Quality\">
#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tSAMPLE"
        .to_string();

    match writeln!(file, "{}", headerstr) {
        Err(why) => panic!("couldn't write to {}: {}", vcf_display, why.description()),
        Ok(_) => {}
    }


    for var in &varlist.lst {
        match interval {
            &Some(ref iv) => {
                if var.chrom != iv.chrom ||
                    var.pos0 < iv.start_pos as usize ||
                    var.pos0 > iv.end_pos as usize {
                    continue;
                }
            }
            &None => {}
        }


        if var.genotype == "0/0".to_string() ||
            var.genotype == "0|0".to_string() ||
            var.ref_allele.len() != 1 ||
            var.var_allele.len() != 1 {
            continue
        }

        match writeln!(file,
                       "{}\t{}\t.\t{}\t{}\t{:.2}\t{}\tDP={};RA={};AA={}\tGT:GQ\t{}:{:.2}",
                       var.chrom,
                       var.pos0 + 1,
                       var.ref_allele,
                       var.var_allele,
                       var.qual,
                       var.filter,
                       var.dp,
                       var.ra,
                       var.aa,
                       var.genotype,
                       var.gq) {
            Err(why) => panic!("couldn't write to {}: {}", vcf_display, why.description()),
            Ok(_) => {}
        }
    }
}
