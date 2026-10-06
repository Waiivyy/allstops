//! Build GTFS zips in memory, for tests and the synthetic network generator.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

/// Zip the given `(file name, contents)` pairs, uncompressed.
pub fn zip_files(files: &[(&str, &str)]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in files {
        w.start_file(*name, opts).expect("start zip entry");
        w.write_all(body.as_bytes()).expect("write zip entry");
    }
    w.finish().expect("finish zip").into_inner()
}

/// The smallest valid feed: two stops, one route, one trip on a weekday
/// calendar. Tests start from this and replace single files.
pub fn minimal_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "agency.txt",
            "agency_id,agency_name,agency_url,agency_timezone\nA,Agency,https://example.org,Europe/Berlin\n".into(),
        ),
        (
            "stops.txt",
            "stop_id,stop_name,stop_lat,stop_lon,location_type,parent_station\n\
             S1,One,48.1,11.5,1,\nS1a,One,48.1,11.5,0,S1\nS2,Two,48.11,11.51,1,\nS2a,Two,48.11,11.51,0,S2\n"
                .into(),
        ),
        (
            "routes.txt",
            "route_id,agency_id,route_short_name,route_long_name,route_type\nR,A,U1,One - Two,1\n".into(),
        ),
        (
            "trips.txt",
            "route_id,service_id,trip_id,trip_headsign\nR,WD,T1,Two\n".into(),
        ),
        (
            "stop_times.txt",
            "trip_id,arrival_time,departure_time,stop_id,stop_sequence\n\
             T1,08:00:00,08:00:00,S1a,1\nT1,08:05:00,08:05:00,S2a,2\n"
                .into(),
        ),
        (
            "calendar.txt",
            "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\n\
             WD,1,1,1,1,1,0,0,20261001,20261213\n"
                .into(),
        ),
    ]
}

/// [`minimal_files`] with some files replaced or added, zipped.
pub fn minimal_with(overrides: &[(&str, &str)]) -> Vec<u8> {
    let mut files = minimal_files();
    for (name, body) in overrides {
        match files.iter_mut().find(|(n, _)| n == name) {
            Some(f) => f.1 = body.to_string(),
            None => files.push((
                // Leak is fine: test fixtures only.
                Box::leak(name.to_string().into_boxed_str()),
                body.to_string(),
            )),
        }
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, b)| (*n, b.as_str())).collect();
    zip_files(&refs)
}
