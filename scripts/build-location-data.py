#!/usr/bin/env python3
"""Generate offline WGS84 administrative polygons. Runtime has no GIS dependencies.

Preferred: --osm-input china-YYMMDD.osm.pbf (Geofabrik, pyosmium + shapely).
Reproducible mirror: --provinces ADM1.geojson --counties ADM3.geojson
                    --names CN.zip (geoBoundaries OSM + GeoNames).
The mirror uses GeoNames administrative codes to group county polygons into cities.
"""
import argparse
import collections
import gzip
import hashlib
import json
import pathlib
import re
import zipfile
from shapely.geometry import shape, mapping, Point
from shapely.ops import unary_union
from shapely.strtree import STRtree

ROOT = pathlib.Path(__file__).resolve().parents[1]
PROVINCES = dict(zip([
    'Hainan','Taiwan','Guangxi','Fujian','Yunnan','Guizhou','Jiangxi','Hunan','Zhejiang','Shanghai',
    'Chongqing','Hubei','Sichuan','Anhui','Jiangsu','Henan','Tibet','Shandong','Qinghai','Ningxia',
    'Shaanxi','Tianjin','Shanxi','Beijing','Gansu','Hebei','Liaoning','Jilin','Xinjiang','Inner',
    'Heilongjiang','Macau','Hong','Guangzhou'],[
    '海南省','台湾省','广西壮族自治区','福建省','云南省','贵州省','江西省','湖南省','浙江省','上海市',
    '重庆市','湖北省','四川省','江苏省','江苏省','河南省','西藏自治区','山东省','青海省','宁夏回族自治区',
    '陕西省','天津市','山西省','北京市','甘肃省','河北省','辽宁省','吉林省','新疆维吾尔自治区','内蒙古自治区',
    '黑龙江省','澳门特别行政区','香港特别行政区','广东省']))
PROVINCES['Anhui'] = '安徽省'


def chinese(entry):
    candidates = [entry[1]] + entry[3].split(',')
    chinese_names = [s for s in candidates if re.fullmatch(r'[\u3400-\u9fff·（）()]+', s)]
    return next((s for s in chinese_names if s.endswith(('省','市','区','县','旗','州'))), chinese_names[0] if chinese_names else entry[1])


def normalized(name):
    name = name.lower().replace(' ', '').replace('-', '').replace("'", '')
    for suffix in ['autonomouscounty','autonomousregion','autonomousprefecture','county','district','prefecture','province','municipality','city','xian','shi','qu']:
        if name.endswith(suffix):
            return name[:-len(suffix)]
    return name


def polygons(geometry):
    if geometry.geom_type == 'Polygon': return [mapping(geometry)['coordinates']]
    if geometry.geom_type == 'MultiPolygon': return mapping(geometry)['coordinates']
    raise ValueError(f'Unexpected administrative geometry: {geometry.geom_type}')


def region(identifier, name, level, geometry):
    geometry = geometry.simplify(0.0002, preserve_topology=True)
    if geometry.is_empty or not geometry.is_valid: raise ValueError(f'Invalid geometry: {identifier}')
    rounded = lambda rings: [[[round(v, 6) for v in point] for point in ring] for ring in rings]
    return dict(id=str(identifier), name=name, level=level,
                bounds=[round(v, 6) for v in geometry.bounds],
                polygons=[rounded(p) for p in polygons(geometry)])


def from_osm(path):
    import osmium
    factory = osmium.geom.GeoJSONFactory()
    class Handler(osmium.SimpleHandler):
        def __init__(self): super().__init__(); self.regions = []
        def area(self, area):
            tags = area.tags
            if tags.get('boundary') != 'administrative': return
            level = tags.get('admin_level')
            if level not in ['4','5','6']: return
            name = tags.get('name:zh') or tags.get('name')
            if not name: return
            geometry = shape(json.loads(factory.create_multipolygon(area)))
            if geometry.is_valid:
                self.regions.append(region(f'osm-{area.orig_id()}', name, int(level), geometry))
    handler = Handler()
    handler.apply_file(str(path), locations=True, idx='flex_mem')
    return handler.regions


def from_mirror(provinces, counties, names):
    with zipfile.ZipFile(names) as archive:
        entries = [line.split('\t') for line in archive.read('CN.txt').decode().splitlines()]
    admin = [e for e in entries if e[6]=='A' and e[7] in ['ADM1','ADM2','ADM3']]
    provinces_names = {e[10]: chinese(e) for e in admin if e[7]=='ADM1'}
    counties_names = [e for e in admin if e[7]=='ADM3']
    tree = STRtree([Point(float(e[5]), float(e[4])) for e in counties_names])
    cities = {(e[10],e[11]): e for e in admin if e[7]=='ADM2'}
    result = []
    for feature in json.loads(provinces.read_text())['features']:
        p = feature['properties']; name = p['shapeName']
        translated = PROVINCES.get(name.split()[0], name)
        result.append(region('province-'+p['shapeID'],translated,4,shape(feature['geometry'])))
    groups = collections.defaultdict(list)
    matched = 0
    for feature in json.loads(counties.read_text())['features']:
        p = feature['properties']; geometry = shape(feature['geometry'])
        if not geometry.is_valid: geometry = geometry.buffer(0)
        candidates = [counties_names[int(i)] for i in tree.query(geometry, predicate='intersects')]
        normalized_name = normalized(p['shapeName'])
        exact = [e for e in candidates if normalized(e[2])==normalized_name or normalized(e[1])==normalized_name]
        # Administrative seats can sit outside their boundary after boundary changes.
        # Do not invent a county/city association if there are multiple unrelated seats.
        chosen = exact[0] if exact else candidates[0] if len(candidates)==1 else None
        name = chinese(chosen) if chosen else p['shapeName']
        if chosen:
            matched += 1
            city_key = (chosen[10],chosen[11])
            if city_key in cities: groups[city_key].append(geometry)
        entry = region('county-'+p['shapeID'],name,6,geometry)
        if chosen:
            ancestors = [provinces_names.get(chosen[10])]
            city = cities.get((chosen[10],chosen[11]))
            if city: ancestors.append(chinese(city))
            entry['ancestors'] = list(dict.fromkeys(n for n in ancestors if n and n != name))
        result.append(entry)
    for key, geometries in groups.items():
        e = cities[key]
        result.append(region('city-'+e[0],chinese(e),5,unary_union(geometries)))
    print(f'Chinese county names matched: {matched}; derived cities: {len(groups)}')
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--osm-input',type=pathlib.Path)
    parser.add_argument('--provinces',type=pathlib.Path)
    parser.add_argument('--counties',type=pathlib.Path)
    parser.add_argument('--names',type=pathlib.Path)
    parser.add_argument('--output',type=pathlib.Path,default=ROOT/'src-tauri/resources/locations/china.json.gz')
    args = parser.parse_args()
    if args.osm_input:
        result = from_osm(args.osm_input); inputs=[args.osm_input]
        source='Geofabrik China OSM extract'
    else:
        if not all([args.provinces,args.counties,args.names]): parser.error('Supply --osm-input or all three mirror inputs')
        result=from_mirror(args.provinces,args.counties,args.names); inputs=[args.provinces,args.counties,args.names]
        source='geoBoundaries 9469f09 OSM county mirror; GeoNames CN administrative names'
    if len(result)<2000: raise ValueError('Administrative coverage unexpectedly incomplete')
    result.sort(key=lambda r:(r['level'],r['id']))
    raw=json.dumps(result,ensure_ascii=False,separators=(',',':')).encode()
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_bytes(gzip.compress(raw,compresslevel=9,mtime=0))
    metadata={'source':source,'coordinateSystem':'WGS84','schemaVersion':1,
              'regions':len(result),'sha256':hashlib.sha256(args.output.read_bytes()).hexdigest(),
              'inputs':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in inputs}}
    args.output.with_name('manifest.json').write_text(json.dumps(metadata,ensure_ascii=False,indent=2)+'\n')
    print(f'Wrote {len(result)} regions, {args.output.stat().st_size:,} bytes')

if __name__=='__main__': main()
