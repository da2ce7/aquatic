//! Benchmark announce and scrape handlers
//! 
//! Example summary output:
//! ```
//! ## Average results over 50 rounds
//! 
//! Connect handler:   2 514 978 requests/second,   397.87 ns/request
//! Announce handler:    246 744 requests/second,  4054.58 ns/request
//! Scrape handler:      499 385 requests/second,  2007.23 ns/request
//! ```

use std::time::{Duration, Instant};
use std::io::Cursor;
use std::net::SocketAddr;

use indicatif::{ProgressBar, ProgressStyle, ProgressIterator};
use num_format::{Locale, ToFormattedString};
use rand::{Rng, thread_rng, rngs::SmallRng, SeedableRng};

use aquatic::common::*;
use aquatic::config::Config;
use bittorrent_udp::converters::*;


mod announce;
mod common;
mod connect;
mod scrape;

use common::*;


#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;


macro_rules! print_results {
    ($request_type:expr, $num_rounds:expr, $data:expr) => {
        let per_second = (
            ($data.0 / ($num_rounds as f64)
        ) as usize).to_formatted_string(&Locale::se);

        println!(
            "{} {:>10} requests/second, {:>8.2} ns/request",
            $request_type,
            per_second,
            $data.1 / ($num_rounds as f64)
        );
    };
}


fn main(){
    let num_rounds = 50;

    let mut connect_data = (0.0, 0.0);
    let mut announce_data = (0.0, 0.0);
    let mut scrape_data = (0.0, 0.0);

    fn create_progress_bar(name: &str, iterations: u64) -> ProgressBar {
        let t = format!("{:<16} {}", name, "{wide_bar} {pos:>2}/{len:>2}");
        let style = ProgressStyle::default_bar().template(&t);

        ProgressBar::new(iterations).with_style(style)
    }

    println!("# Benchmarking request handlers\n");

    {
        let requests = connect::create_requests();

        let requests: Vec<([u8; MAX_REQUEST_BYTES], SocketAddr)> = requests.into_iter()
            .map(|(request, src)| {
                let mut buffer = [0u8; MAX_REQUEST_BYTES];
                let mut cursor = Cursor::new(buffer.as_mut());

                request_to_bytes(&mut cursor, Request::Connect(request));

                (buffer, src)
            })
            .collect();

        ::std::thread::sleep(Duration::from_secs(1));

        let pb = create_progress_bar("Connect handler", num_rounds);

        for _ in (0..num_rounds).progress_with(pb){
            let requests = requests.clone();

            ::std::thread::sleep(Duration::from_millis(200));

            let d = connect::bench(requests);

            ::std::thread::sleep(Duration::from_millis(200));

            connect_data.0 += d.0;
            connect_data.1 += d.1;
        }
    }

    let mut rng = SmallRng::from_rng(thread_rng()).unwrap();
    let info_hashes = create_info_hashes(&mut rng);
    let config = Config::default();

    let state_for_scrape: State = {
        let requests = announce::create_requests(
            &mut rng,
            &info_hashes
        );

        let state = State::new();

        let time = Time(Instant::now());

        for (request, src) in requests.iter() {
            let key = ConnectionKey {
                connection_id: request.connection_id,
                socket_addr: *src,
            };

            state.connections.insert(key, time);
        }

        let requests: Vec<([u8; MAX_REQUEST_BYTES], SocketAddr)> = requests.into_iter()
            .map(|(request, src)| {
                let mut buffer = [0u8; MAX_REQUEST_BYTES];
                let mut cursor = Cursor::new(buffer.as_mut());

                request_to_bytes(&mut cursor, Request::Announce(request));

                (buffer, src)
            })
            .collect();

        let mut state_for_scrape = State::new();

        ::std::thread::sleep(Duration::from_secs(1));

        let pb = create_progress_bar("Announce handler", num_rounds);

        for round in (0..num_rounds).progress_with(pb) {
            let requests = requests.clone();

            ::std::thread::sleep(Duration::from_millis(200));

            let d = announce::bench(&state, &config, requests);

            ::std::thread::sleep(Duration::from_millis(200));

            announce_data.0 += d.0;
            announce_data.1 += d.1;

            if round == num_rounds - 1 {
                state_for_scrape = state.clone();
            }
        }

        state_for_scrape
    };

    state_for_scrape.connections.clear();

    {
        let state = state_for_scrape;

        let requests = scrape::create_requests(&mut rng, &info_hashes);

        let time = Time(Instant::now());

        for (request, src) in requests.iter() {
            let key = ConnectionKey {
                connection_id: request.connection_id,
                socket_addr: *src,
            };

            state.connections.insert(key, time);
        }

        let requests: Vec<([u8; MAX_REQUEST_BYTES], SocketAddr)> = requests.into_iter()
            .map(|(request, src)| {
                let mut buffer = [0u8; MAX_REQUEST_BYTES];
                let mut cursor = Cursor::new(buffer.as_mut());

                request_to_bytes(&mut cursor, Request::Scrape(request));

                (buffer, src)
            })
            .collect();

        ::std::thread::sleep(Duration::from_secs(1));

        let pb = create_progress_bar("Scrape handler", num_rounds);

        for _ in (0..num_rounds).progress_with(pb) {
            let requests = requests.clone();

            ::std::thread::sleep(Duration::from_millis(200));

            let d = scrape::bench(&state, requests);

            ::std::thread::sleep(Duration::from_millis(200));

            scrape_data.0 += d.0;
            scrape_data.1 += d.1;
        }
    }

    println!("\n## Average results over {} rounds\n", num_rounds);

    print_results!("Connect handler: ", num_rounds, connect_data);
    print_results!("Announce handler:", num_rounds, announce_data);
    print_results!("Scrape handler:  ", num_rounds, scrape_data);
}


fn create_info_hashes(rng: &mut impl Rng) -> Vec<InfoHash> {
    let mut info_hashes = Vec::new();

    for _ in 0..common::NUM_INFO_HASHES {
        info_hashes.push(InfoHash(rng.gen()));
    }

    info_hashes
}