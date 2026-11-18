# Offline maps and location data

PhotoSys reads WGS84 coordinates locally. Browsing and reverse lookup do not call a
geocoding service. A map download sends only the explicitly selected region to
Protomaps, not media files, filenames or the media catalogue.

## Bundled administrative lookup

The preferred regeneration input is a dated Geofabrik China OSM extract:
https://download.geofabrik.de/asia/china.html. That download endpoint was unreachable
in the implementation environment. The checked-in snapshot therefore uses these
fixed OSM boundaries and Chinese administrative names:

- geoBoundaries commit `9469f09`, CHN ADM3, 2017 county boundaries derived from
  Lee Beryman / OpenStreetMap, under Open Data Commons Open Database License 1.0.
  https://www.geoboundaries.org/api/current/gbOpen/CHN/ADM3/
- geoBoundaries commit `9469f09`, CHN ADM1 province overview, 2019, public domain.
  https://www.geoboundaries.org/api/current/gbOpen/CHN/ADM1/
- GeoNames `CN.zip`, retrieved 2026-11-19, for Chinese names and administrative
  parents. Attribution: GeoNames, https://www.geonames.org/.
  https://download.geonames.org/export/dump/ and https://www.geonames.org/about.html
  Creative Commons Attribution, https://creativecommons.org/licenses/by/4.0/.

`src-tauri/resources/locations/manifest.json` records exact input SHA-256 hashes and
the generated archive hash. The distributed lookup data in `china.json.gz` and the
regeneration script are available in this source tree. The lookup database is an
ODbL derived database; OSM attribution and the ODbL terms apply to it:
https://opendatacommons.org/licenses/odbl/1-0/.

The snapshot has 34 province overviews, 2,864 county polygons, and 336 city groups
(3,234 total). Chinese names match 2,724 counties. County parent codes take
precedence over coarse province outlines, particularly near Hong Kong and Shenzhen.
Unmatched names may remain in the source language. City outlines are unions of
matched counties, not a complete authoritative administrative dataset. The data
represents its source snapshot, not current administrative changes or street
addresses. Missing levels are omitted. Overseas points retain their coordinates.

Regenerate from Geofabrik (install pyosmium and shapely in a build environment):

```sh
python scripts/build-location-data.py --osm-input china-YYMMDD.osm.pbf
```

Reproduce the bundled mirror snapshot with the input files whose hashes are in the
manifest:

```sh
python scripts/build-location-data.py --provinces ADM1.geojson --counties ADM3.geojson --names CN.zip
```

## Offline street maps and assets

- Protomaps basemap: © OpenStreetMap contributors, ODbL Produced Work;
  https://docs.protomaps.com/basemaps/downloads/ and
  https://www.openstreetmap.org/copyright. Regional archives are downloaded only
  after an explicit action. Local imports must use PMTiles v3 with Protomaps v4
  layer metadata and uncompressed/gzip internal and tile compression.
- PMTiles CLI `1.31.2`: BSD-3-Clause, upstream source
  https://github.com/protomaps/go-pmtiles/tree/v1.31.2.
  The full license is bundled in `resources/PMTILES-LICENSE.txt`.
  `prepare-offline-maps.py` pins release archive SHA-256 values for each target.
- Basemap font glyphs and sprites: protomaps/basemaps-assets commit
  `028c18f713baecad011301ff7a69acc39bcc2ae7`. Noto Sans glyphs use SIL Open Font
  License 1.1; see `public/maps/fonts/OFL.txt`. Sprite assets are from the
  Protomaps basemap BSD-3-Clause project.
- Bundled Noto Sans SC variable font from google/fonts `ofl/notosanssc`,
  SIL Open Font License 1.1, see `public/maps/CHINESE-FONT-OFL.txt`.
- World land overview: Natural Earth `ne_110m_land`, public domain,
  https://www.naturalearthdata.com/about/terms-of-use/.
- MapLibre GL JS: BSD-3-Clause; PMTiles JS: BSD-3-Clause;
  @protomaps/basemaps: BSD-3-Clause. Their license texts are distributed under `resources/licenses/`.

Full font and resource checksums are in `public/maps/manifest.json`. All map styles,
workers, glyphs, sprites, Chinese fonts and the overview load locally. No map API
key, running tile server or external geocoding service is required.
