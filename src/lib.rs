//! `kontext`: project memory for coding agents. See DESIGN.md.
//!
//! The library is used by the binary and the tests; the later phases extend it
//! (views, tasks, synchronization).

pub mod app;
pub mod board;
pub mod capture;
pub mod changelog;
pub mod cli;
pub mod config;
pub mod cursor;
pub mod distill;
pub mod doctor;
pub mod facts;
pub mod features;
pub mod find;
pub mod git;
pub mod hlc;
pub mod hook;
pub mod ideas;
pub mod import_v0;
pub mod inbox;
pub mod index;
pub mod install;
pub mod measure;
pub mod media;
pub mod model;
pub mod plan;
pub mod recall;
pub mod report;
pub mod repos;
pub mod scope;
pub mod sign;
pub mod state;
pub mod store;
pub mod sync;
pub mod tasks;
pub mod tree;
pub mod ui;
pub mod verify;
pub mod views;
pub mod write;
