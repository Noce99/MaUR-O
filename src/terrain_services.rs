//! Every elevation service [`mauro terrain`](crate::terrain) knows of.
//!
//! Each was checked by hand before going in: a box of a known place fetched
//! in more than one CRS and its elevations looked at, what it sends where it
//! has none seen, and its licence found. None needs a key or an account.
//!
//! The order matters: where two cover the same ground, the first one with
//! elevations of the whole map is used. Finer before coarser, newer surveys
//! before older, a region before its country.
//!
//! Left out on purpose: the region's river-corridor surveys
//! (`Dtm_APC_*`, `Dtm_cgr_emg_fiumi_202305`), too narrow to hold a map, the
//! national 1 m LiDAR its server also serves (`Dtm1x1PST`), licensed for
//! institutional use only, and its `Dtm10k_ed2014` WCS, which is a coloured
//! picture of the terrain, not elevations.

use crate::orthophoto::LatLonBox;
use crate::terrain::{Protocol, TerrainService};

/// The region of Emilia-Romagna's ArcGIS server, up to its service names.
macro_rules! rer {
    ($name:literal) => {
        concat!(
            "https://servizigis.regione.emilia-romagna.it/arcgis/rest/services/public/",
            $name,
            "/ImageServer"
        )
    };
}

/// The region of Emilia-Romagna's own terms for its open data.
const RER_LICENCE: &str = "CC BY 4.0";

/// Every service, best first.
pub const TERRAIN_SERVICES: &[TerrainService] = &[
    TerrainService {
        id: "it-er-lidar-2025-26",
        area: "Emilia-Romagna, Italy (LiDAR, survey under way)",
        years: "2025–2026",
        url: rer!("DtmRER2025_26"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 0.5,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 10.10,
            min_lat: 43.73,
            max_lon: 12.23,
            max_lat: 45.04,
        },
        attribution: "DTM RER 2025-26, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-modena-2024",
        area: "Modena (municipality), Italy (LiDAR)",
        years: "2024",
        url: rer!("Dtm_comune_MO_2024"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 0.5,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 10.76,
            min_lat: 44.54,
            max_lon: 11.04,
            max_lat: 44.76,
        },
        attribution: "DTM Comune di Modena 2024, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-er-lidar-2023-24",
        area: "Emilia-Romagna, Italy (LiDAR)",
        years: "2023–2024",
        url: rer!("DtmRER2023_24"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 0.5,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 9.29,
            min_lat: 43.81,
            max_lon: 12.84,
            max_lat: 45.15,
        },
        attribution: "DTM RER 2023-24, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-bologna-2023",
        area: "Bologna (municipality), Italy (LiDAR)",
        years: "2023",
        url: rer!("Dtm_comune_BO_2023"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 0.5,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 11.22,
            min_lat: 44.41,
            max_lon: 11.45,
            max_lat: 44.57,
        },
        attribution: "DTM Comune di Bologna 2023, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-er-lidar-2022",
        area: "Emilia-Romagna, Italy (LiDAR)",
        years: "2022",
        url: rer!("DtmRER2022"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 0.5,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 10.30,
            min_lat: 43.71,
            max_lon: 12.62,
            max_lat: 44.99,
        },
        attribution: "DTM RER 2022, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-er-dtm-5m",
        area: "Emilia-Romagna, Italy (all of it)",
        years: "before 2017",
        url: rer!("Dtm5x5"),
        protocol: Protocol::ArcGisImageServer,
        ground_resolution: 5.0,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 9.09,
            min_lat: 43.67,
            max_lon: 12.86,
            max_lat: 45.18,
        },
        attribution: "DTM 5x5, Regione Emilia-Romagna",
        licence: RER_LICENCE,
    },
    TerrainService {
        id: "it-tinitaly",
        area: "Italy (TINITALY 1.1)",
        years: "2023",
        url: "https://tinitaly.pi.ingv.it/TINItaly_1_1/wcs",
        protocol: Protocol::Wcs {
            coverage_id: "TINItaly_1_1__tinitaly_dem",
        },
        ground_resolution: 10.0,
        max_tile_size: 2000,
        coverage: LatLonBox {
            min_lon: 6.5,
            min_lat: 35.4,
            max_lon: 18.6,
            max_lat: 47.1,
        },
        attribution: "Tarquini S. et al. (2023), TINITALY 1.1, INGV, doi:10.13127/tinitaly/1.1",
        licence: "CC BY 4.0",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = TERRAIN_SERVICES.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TERRAIN_SERVICES.len());
    }

    #[test]
    fn bologna_is_covered_finest_first_and_tinitaly_last() {
        let (lat, lon) = (44.494, 11.343);
        let covering: Vec<&str> = TERRAIN_SERVICES
            .iter()
            .filter(|s| s.covers(lat, lon))
            .map(|s| s.id)
            .collect();
        assert_eq!(covering.first(), Some(&"it-er-lidar-2025-26"));
        assert!(covering.contains(&"it-bologna-2023"));
        assert_eq!(covering.last(), Some(&"it-tinitaly"));
        let resolutions: Vec<f64> = TERRAIN_SERVICES
            .iter()
            .map(|s| s.ground_resolution)
            .collect();
        assert!(resolutions.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn only_tinitaly_covers_rome() {
        let covering: Vec<&str> = TERRAIN_SERVICES
            .iter()
            .filter(|s| s.covers(41.9, 12.5))
            .map(|s| s.id)
            .collect();
        assert_eq!(covering, ["it-tinitaly"]);
    }
}
