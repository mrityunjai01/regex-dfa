// Copyright 2015 Joe Neeman.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use bit_set::BitSet;
use builder::NfaBuilder;
use dfa::Dfa;
use error;
use regex_syntax;
use std;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::mem;
use std::result::Result;
use transition::{NfaTransitions, Predicate, SymbRange};


#[derive(PartialEq, Debug)]
pub struct NfaState {
    pub transitions: NfaTransitions,
    pub accepting: bool,
}

impl NfaState {
    pub fn new(accepting: bool) -> NfaState {
        NfaState {
            transitions: NfaTransitions::new(),
            accepting: accepting,
        }
    }
}

/// `Nfa` represents a non-deterministic finite automaton. We do not provide any support for
/// actually executing the automaton directly; its main purpose is to turn into a `Dfa`.
///
/// By default, `Nfa` represents an "unanchored" automaton, meaning that if we were to execute
/// it on some input then it could match any subset of the input, not just the part starting at
/// the beginning. In terms of regexes, it's like having an implicit ".*" at the start.
///
/// The initial state of an `Nfa` is always state zero, but see also the documentation for
/// `anchored_states`.
#[derive(PartialEq)]
pub struct Nfa {
    states: Vec<NfaState>,

    /// Sometimes we want to only match at the beginning of the text; we can represent this
    /// using `anchored_states`, which is a set of states that are all valid as starting states,
    /// but only if we start matching at the beginning of the input.
    ///
    /// Note that `transition::Predicate` provides another, higher-level, way to represent the same
    /// information. Before turning this `Nfa` into a `Dfa`, we will lower the
    /// `transition::Predicate` representation into this one.
    anchored_states: BitSet,
}

impl Debug for Nfa {
    fn fmt(&self, f: &mut Formatter) -> Result<(), std::fmt::Error> {
        try!(f.write_fmt(format_args!("Nfa ({} states):\n", self.states.len())));

        for (st_idx, st) in self.states.iter().enumerate() {
            try!(f.write_fmt(format_args!("\tState {} (accepting: {}):\n", st_idx, st.accepting)));

            if !st.transitions.ranges.is_empty() {
                try!(f.write_str("\t\tTransitions:\n"));
                for &(range, target) in &st.transitions.ranges {
                    try!(f.write_fmt(format_args!("\t\t\t{} -- {} => {}\n",
                                                  range.from, range.to, target)));
                }
            }

            if !st.transitions.eps.is_empty() {
                try!(f.write_fmt(format_args!("\t\tEps-transitions: {:?}\n", &st.transitions.eps)));
            }
        }
        Ok(())
    }
}

impl Nfa {
    pub fn new() -> Nfa {
        Nfa {
            states: Vec::new(),
            anchored_states: BitSet::new(),
        }
    }

    pub fn num_states(&self) -> usize {
        self.states.len()
    }

    pub fn from_regex(re: &str) -> Result<Nfa, error::Error> {
        let expr = try!(regex_syntax::Expr::parse(re));
        Ok(NfaBuilder::from_expr(&expr).to_automaton())
    }

    pub fn with_capacity(n: usize) -> Nfa {
        Nfa {
            states: Vec::with_capacity(n),
            anchored_states: BitSet::with_capacity(n),
        }
    }

    pub fn add_transition(&mut self, from: usize, to: usize, r: SymbRange) {
        self.states[from].transitions.ranges.push((r, to));
    }

    pub fn add_state(&mut self, accepting: bool) {
        self.states.push(NfaState::new(accepting));
    }

    pub fn add_eps(&mut self, from: usize, to: usize) {
        self.states[from].transitions.eps.push(to);
    }

    pub fn add_predicate(&mut self, from: usize, to: usize, pred: Predicate) {
        self.states[from].transitions.predicates.push((pred, to));
    }

    /// Returns the list of all input-consuming transitions from the given state.
    ///
    /// TODO: this would be a prime candidate for using abstract return types, if that ever lands.
    pub fn transitions_from(&self, from: usize) -> &Vec<(SymbRange, usize)> {
        &self.states[from].transitions.ranges
    }

    /// Modifies this automaton to remove all transition predicates.
    pub fn remove_predicates(&mut self) {
        while self.remove_predicates_once() {}
    }
    // This is the algorithm for removing predicates, which we run repeatedly until
    // we reach a fixed point.
    //  for every predicate {
    //      suppose the predicate goes from state a to state b
    //      make a new state
    //      for every transition or predicate leading into a {
    //          make a copy of that transition leading into the new state
    //      }
    //      for every transition or predicate leading out of b {
    //          make a copy of that transition leading out of the new state
    //      }
    //  }
    // Above, when we say "leading into" or "leading out of," that includes eps-closures.
    fn remove_predicates_once(&mut self) -> bool{
        let orig_len = self.states.len();
        let mut reversed = self.reversed();

        for idx in 0..orig_len {
            let preds = self.states[idx].transitions.predicates.clone();
            self.states[idx].transitions.predicates.clear();
            // Also remove the preds from our reversed copy.
            for (pred_idx, &(_, target)) in preds.iter().enumerate() {
                reversed.states[target].transitions.predicates.remove(pred_idx);
            }

            for &(ref pred, pred_target_idx) in &preds {
                self.states.push(NfaState::new(false));
                reversed.states.push(NfaState::new(false));
                let new_idx = self.states.len() - 1;

                let in_states = reversed.eps_closure_single(idx);
                let out_states = self.eps_closure_single(pred_target_idx);
                let (in_trans, out_trans) =
                    pred.filter_transitions(&reversed.transitions(&in_states),
                                            &self.transitions(&out_states));

                for (range, ref sources) in in_trans {
                    for source in sources {
                        self.add_transition(source, new_idx, range);
                        reversed.add_transition(new_idx, source, range);
                    }
                }
                for (other_pred, source) in reversed.predicates(&in_states) {
                    if let Some(p) = pred.intersect(&other_pred) {
                        self.add_predicate(source, new_idx, p.clone());
                        reversed.add_predicate(new_idx, source, p);
                    }
                }
                for (range, ref targets) in out_trans {
                    for target in targets {
                        self.add_transition(new_idx, target, range);
                        reversed.add_transition(target, new_idx, range);
                    }
                }
                for (other_pred, target) in self.predicates(&out_states) {
                    if let Some(p) = pred.intersect(&other_pred) {
                        self.add_predicate(new_idx, target, p.clone());
                        reversed.add_predicate(target, new_idx, p);
                    }
                }
            }
        }

        self.states.len() > orig_len
    }

    /// Returns a copy with all transitions reversed.
    ///
    /// Its states will have the same indices as those of the original.
    fn reversed(&self) -> Nfa {
        let mut ret = Nfa::with_capacity(self.states.len());

        for st in self.states.iter() {
            ret.states.push(NfaState::new(st.accepting));
        }

        for (idx, st) in self.states.iter().enumerate() {
            for &(ref range, target) in st.transitions.ranges.iter() {
                ret.states[target].transitions.ranges.push((*range, idx));
            }
            for &target in st.transitions.eps.iter() {
                ret.states[target].transitions.eps.push(idx);
            }
            for &(ref pred, target) in st.transitions.predicates.iter() {
                ret.states[target].transitions.predicates.push((pred.clone(), target));
            }
        }

        ret
    }

    /// Creates a deterministic automaton representing the same language.
    ///
    /// This assumes that we have no transition predicates -- if there are any, you must call
    /// `remove_predicates` before calling `determinize`.
    pub fn determinize(&self) -> Dfa {
        let mut ret = Dfa::new();
        let mut state_map = HashMap::<BitSet, usize>::new();
        let mut active_states = Vec::<BitSet>::new();
        let start_state = self.eps_closure_single(0);

        ret.add_state(self.accepting(&start_state));
        active_states.push(start_state.clone());
        state_map.insert(start_state, 0);

        while active_states.len() > 0 {
            let state = active_states.pop().unwrap();
            let state_idx = *state_map.get(&state).unwrap();
            let trans = self.transitions(&state);
            for (range, target) in trans.into_iter() {
                let target_idx = if state_map.contains_key(&target) {
                        *state_map.get(&target).unwrap()
                    } else {
                        ret.add_state(self.accepting(&target));
                        active_states.push(target.clone());
                        state_map.insert(target, ret.num_states() - 1);
                        ret.num_states() - 1
                    };
                ret.add_transition(state_idx, target_idx, range);
            }
        }

        ret.sort_transitions();
        ret
    }

    fn eps_closure(&self, states: &BitSet) -> BitSet {
        let mut ret = states.clone();
        let mut new_states = states.clone();
        let mut next_states = BitSet::with_capacity(self.states.len());
        loop {
            for s in &new_states {
                for &t in &self.states[s].transitions.eps {
                    next_states.insert(t);
                }
            }

            if next_states.is_subset(&ret) {
                return ret;
            } else {
                next_states.difference_with(&ret);
                ret.union_with(&next_states);
                mem::swap(&mut next_states, &mut new_states);
                next_states.clear();
            }
        }
    }

    fn eps_closure_single(&self, state: usize) -> BitSet {
        let mut set = BitSet::with_capacity(self.states.len());
        set.insert(state);
        self.eps_closure(&set)
    }

    fn accepting(&self, states: &BitSet) -> bool {
        states.iter().any(|s| { self.states[s].accepting })
    }

    /// Finds all the transitions out of the given set of states.
    ///
    /// Only transitions that consume output are returned. In particular, you
    /// probably want `states` to already be eps-closed.
    fn transitions(&self, states: &BitSet) -> Vec<(SymbRange, BitSet)> {
        let trans = states.iter()
                          .flat_map(|s| self.states[s].transitions.ranges.iter().cloned())
                          .collect();
        let trans = NfaTransitions::from_vec(trans).collect_transition_pairs();

        trans.into_iter().map(|x| (x.0, self.eps_closure(&x.1))).collect()
    }

    /// Finds all predicates transitioning out of the given set of states.
    fn predicates(&self, states: &BitSet) -> Vec<(Predicate, usize)> {
        states.iter()
              .flat_map(|s| self.states[s].transitions.predicates.iter().cloned())
              .collect()
    }
}


