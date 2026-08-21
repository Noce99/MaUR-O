//! OCD coordinate reference systems: the grid and zone a file names, as an
//! EPSG code.
//!
//! An OCD file does not carry a projection. It carries a number -- the `i`
//! field of its type 1039 parameter string -- which names a grid and a zone
//! within it, and the meaning of that number is a table OCAD keeps and the
//! file format does not explain. This is that table.
//!
//! Generated from OpenOrienteering Mapper's
//! `src/fileformats/ocd_georef_fields.cpp` (its `grid_zone_epsg_codes`).
//! UTM (grid 2) and German Gauss-Krueger (grid 8) are computed rather than
//! listed, in [`epsg_from_grid_zone`](crate::ocd::epsg_from_grid_zone), and so
//! do not appear here.

/// Combined grid and zone code, as the file gives it, paired with its EPSG
/// code. Sorted by the code, so that it can be searched without being loaded.
static GRID_ZONE_EPSG: &[(i32, u32)] = &[
    (3028, 31257),   // MGI / Austria GK M28
    (3031, 31258),   // MGI / Austria GK M31
    (3034, 31259),   // MGI / Austria GK M34
    (3035, 31254),   // MGI / Austria GK West
    (3036, 31255),   // MGI / Austria GK Central
    (3038, 31256),   // MGI / Austria GK East
    (4001, 31300),   // Belge 1972 / Belge Lambert 72
    (4002, 3447),    // ETRS89 / Belgian Lambert 2005
    (4003, 3812),    // ETRS89 / Belgian Lambert 2008
    (5000, 27700),   // OSGB 1936
    (6001, 2391),    // KKJ / Finland zone 1
    (6002, 2392),    // KKJ / Finland zone 2
    (6003, 2393),    // KKJ / Finland Uniform Coordinate System
    (6004, 2394),    // KKJ / Finland zone 4
    (6005, 3067),    // TM35FIN, ETRS89 / ETRS-TM35FIN
    (6006, 3873),    // ETRS89 / GK19FIN
    (6007, 3874),    // ETRS89 / GK20FIN
    (6008, 3875),    // ETRS89 / GK21FIN
    (6009, 3876),    // ETRS89 / GK22FIN
    (6010, 3877),    // ETRS89 / GK23FIN
    (6011, 3878),    // ETRS89 / GK24FIN
    (6012, 3879),    // ETRS89 / GK25FIN
    (6013, 3880),    // ETRS89 / GK26FIN
    (6014, 3881),    // ETRS89 / GK27FIN
    (6015, 3882),    // ETRS89 / GK28FIN
    (6016, 3883),    // ETRS89 / GK29FIN
    (6017, 3884),    // ETRS89 / GK30FIN
    (6018, 3885),    // ETRS89 / GK31FIN
    (6019, 3126),    // ETRS89 / ETRS-GK19FIN
    (6020, 3127),    // ETRS89 / ETRS-GK20FIN
    (6021, 3128),    // ETRS89 / ETRS-GK21FIN
    (6022, 3129),    // ETRS89 / ETRS-GK22FIN
    (6023, 3130),    // ETRS89 / ETRS-GK23FIN
    (6024, 3131),    // ETRS89 / ETRS-GK24FIN
    (6025, 3132),    // ETRS89 / ETRS-GK25FIN
    (6026, 3133),    // ETRS89 / ETRS-GK26FIN
    (6027, 3134),    // ETRS89 / ETRS-GK27FIN
    (6028, 3135),    // ETRS89 / ETRS-GK28FIN
    (6029, 3136),    // ETRS89 / ETRS-GK29FIN
    (6030, 3137),    // ETRS89 / ETRS-GK30FIN
    (6031, 3138),    // ETRS89 / ETRS-GK31FIN
    (6032, 3387),    // KKJ / Finland zone 5
    (7001, 27591),   // NTF_Paris_Nord_France
    (7002, 27581),   // NTF_Paris_France_I
    (7003, 27592),   // NTF_Paris_Centre_France
    (7004, 27572),   // NTF (Paris) / Lambert zone II
    (7005, 27593),   // NTF_Paris_Sud_France
    (7006, 27583),   // NTF_Paris_France_III
    (7007, 27594),   // NTF_Paris_Corse
    (7008, 27584),   // NTF_Paris_France_IV
    (7009, 2154),    // RGF93 / Lambert-93
    (7010, 3942),    // RGF93 / CC42
    (7011, 3943),    // RGF93 / CC43
    (7012, 3944),    // RGF93 / CC44
    (7013, 3945),    // RGF93 / CC45
    (7014, 3946),    // RGF93 / CC46
    (7015, 3947),    // RGF93 / CC47
    (7016, 3948),    // RGF93 / CC48
    (7017, 3949),    // RGF93 / CC49
    (7018, 3950),    // RGF93 / CC50
    (8006, 25831),   // ETRS89 / UTM zone 31N
    (8007, 25832),   // ETRS89 / UTM zone 32N
    (8008, 25833),   // ETRS89 / UTM zone 33N
    (8013, 3068),    // DHDN / Soldner Berlin
    (8014, 3068),    // DHDN / Soldner Berlin
    (9001, 29902),   // TM65 / Irish Grid
    (9002, 29903),   // TM75 / Irish Grid
    (9003, 2157),    // IRENET95 / Irish Transverse Mercator
    (11001, 30161),  // Tokyo / Japan Plane Rectangular CS I
    (11002, 30162),  // Tokyo / Japan Plane Rectangular CS II
    (11003, 30163),  // Tokyo / Japan Plane Rectangular CS III
    (11004, 30164),  // Tokyo / Japan Plane Rectangular CS IV
    (11005, 30165),  // Tokyo / Japan Plane Rectangular CS V
    (11006, 30166),  // Tokyo / Japan Plane Rectangular CS VI
    (11007, 30167),  // Tokyo / Japan Plane Rectangular CS VII
    (11008, 30168),  // Tokyo / Japan Plane Rectangular CS VIII
    (11009, 30169),  // Tokyo / Japan Plane Rectangular CS IX
    (11010, 30170),  // Tokyo / Japan Plane Rectangular CS X
    (11011, 30171),  // Tokyo / Japan Plane Rectangular CS XI
    (11012, 30172),  // Tokyo / Japan Plane Rectangular CS XII
    (11013, 30173),  // Tokyo / Japan Plane Rectangular CS XIII
    (11014, 30174),  // Tokyo / Japan Plane Rectangular CS XIV
    (11015, 30175),  // Tokyo / Japan Plane Rectangular CS XV
    (11016, 30176),  // Tokyo / Japan Plane Rectangular CS XVI
    (11017, 30177),  // Tokyo / Japan Plane Rectangular CS XVII
    (11018, 30178),  // Tokyo / Japan Plane Rectangular CS XVIII
    (11019, 30179),  // Tokyo / Japan Plane Rectangular CS XIX
    (11020, 2443),   // JGD2000 / Japan Plane Rectangular CS I
    (11021, 2444),   // JGD2000 / Japan Plane Rectangular CS II
    (11022, 2445),   // JGD2000 / Japan Plane Rectangular CS III
    (11023, 2446),   // JGD2000 / Japan Plane Rectangular CS IV
    (11024, 2447),   // JGD2000 / Japan Plane Rectangular CS V
    (11025, 2448),   // JGD2000 / Japan Plane Rectangular CS VI
    (11026, 2449),   // JGD2000 / Japan Plane Rectangular CS VII
    (11027, 2450),   // JGD2000 / Japan Plane Rectangular CS VIII
    (11028, 2451),   // JGD2000 / Japan Plane Rectangular CS IX
    (11029, 2452),   // JGD2000 / Japan Plane Rectangular CS X
    (11030, 2453),   // JGD2000 / Japan Plane Rectangular CS XI
    (11031, 2454),   // JGD2000 / Japan Plane Rectangular CS XII
    (11032, 2455),   // JGD2000 / Japan Plane Rectangular CS XIII
    (11033, 2456),   // JGD2000 / Japan Plane Rectangular CS XIV
    (11034, 2457),   // JGD2000 / Japan Plane Rectangular CS XV
    (11035, 2458),   // JGD2000 / Japan Plane Rectangular CS XVI
    (11036, 2459),   // JGD2000 / Japan Plane Rectangular CS XVII
    (11037, 2460),   // JGD2000 / Japan Plane Rectangular CS XVIII
    (11039, 2461),   // JGD2000 / Japan Plane Rectangular CS XIX
    (12001, 27391),  // NGO 1948 (Oslo) / NGO zone I
    (12002, 27392),  // NGO 1948 (Oslo) / NGO zone II
    (12003, 27393),  // NGO 1948 (Oslo) / NGO zone III
    (12004, 27394),  // NGO 1948 (Oslo) / NGO zone IV
    (12005, 27395),  // NGO 1948 (Oslo) / NGO zone V
    (12006, 27396),  // NGO 1948 (Oslo) / NGO zone VI
    (12007, 27397),  // NGO 1948 (Oslo) / NGO zone VII
    (12008, 27398),  // NGO 1948 (Oslo) / NGO zone VIII
    (12009, 23031),  // ED50 / UTM zone 31N
    (12010, 23032),  // ED50 / UTM zone 32N
    (12011, 23033),  // ED50 / UTM zone 33N
    (12012, 23034),  // ED50 / UTM zone 34N
    (12013, 23035),  // ED50 / UTM zone 35N
    (12014, 23036),  // ED50 / UTM zone 36N
    (13001, 6124),   // WGS 84 / EPSG Arctic zone 4-12
    (13002, 3006),   // SWEREF99 TM
    (13003, 3007),   // SWEREF99 12 00
    (13004, 3008),   // SWEREF99 13 30
    (13005, 3009),   // SWEREF99 15 00
    (13006, 3010),   // SWEREF99 16 30
    (13007, 3011),   // SWEREF99 18 00
    (13008, 3012),   // SWEREF99 14 15
    (13009, 3013),   // SWEREF99 15 45
    (13010, 3014),   // SWEREF99 17 15
    (13011, 3015),   // SWEREF99 18 45
    (13012, 3016),   // SWEREF99 20 15
    (13013, 3017),   // SWEREF99 21 45
    (13014, 3018),   // SWEREF99 23 15
    (14001, 21781),  // CH1903 / LV03 -- Swiss CH1903 / LV03
    (14002, 2056),   // CH1903+ / LV95 -- Swiss CH1903+ / LV95
    (15001, 3912),   // MGI 1901 / Slovene National Grid
    (15002, 3794),   // Slovenia 1996 / Slovene National Grid
    (16001, 4685),   // { OcdGrid::Italy, 2, 4686 }, // MAGNA-SIRGAS
    (16003, 23031),  // ED50 / UTM zone 31N
    (16004, 23032),  // ED50 / UTM zone 32N
    (16005, 23033),  // ED50 / UTM zone 33N
    (16006, 23034),  // ED50 / UTM zone 34N
    (16007, 25832),  // ETRS89 / UTM zone 32N
    (16008, 25833),  // ETRS89 / UTM zone 33N
    (16009, 6707),   // RDN2008 / UTM zone 32N (N-E)
    (16010, 6708),   // RDN2008 / UTM zone 33N (N-E)
    (16011, 6875),   // RDN2008 / Italy zone (N-E)
    (16012, 6876),   // RDN2008 / Zone 12 (N-E)
    (17001, 2206),   // ED50 / 3-degree Gauss-Kruger zone 9
    (17002, 2207),   // ED50 / 3-degree Gauss-Kruger zone 10
    (17003, 2208),   // ED50 / 3-degree Gauss-Kruger zone 11
    (17004, 2209),   // ED50 / 3-degree Gauss-Kruger zone 12
    (17005, 2210),   // ED50 / 3-degree Gauss-Kruger zone 13
    (17006, 2211),   // ED50 / 3-degree Gauss-Kruger zone 14
    (17007, 2212),   // ED50 / 3-degree Gauss-Kruger zone 15
    (17008, 7006),   // Nahrwan 1934 / UTM zone 38N
    (18001, 2046),   // Hartebeesthoek94 / Lo15
    (18002, 2047),   // Hartebeesthoek94 / Lo17
    (18003, 2048),   // Hartebeesthoek94 / Lo19
    (18004, 2049),   // Hartebeesthoek94 / Lo21
    (18005, 2050),   // Hartebeesthoek94 / Lo23
    (18006, 2051),   // Hartebeesthoek94 / Lo25
    (18007, 2052),   // Hartebeesthoek94 / Lo27
    (18008, 2053),   // Hartebeesthoek94 / Lo29
    (18009, 2054),   // Hartebeesthoek94 / Lo31
    (18010, 2055),   // Hartebeesthoek94 / Lo33
    (19000, 2193),   // NZGD2000 / New Zealand Transverse Mercator 2000
    (20001, 28348),  // GDA94 / MGA zone 48
    (20002, 28349),  // GDA94 / MGA zone 49
    (20003, 28350),  // GDA94 / MGA zone 50
    (20004, 28351),  // GDA94 / MGA zone 51
    (20005, 28352),  // GDA94 / MGA zone 52
    (20006, 28353),  // GDA94 / MGA zone 53
    (20007, 28354),  // GDA94 / MGA zone 54
    (20008, 28355),  // GDA94 / MGA zone 55
    (20009, 28356),  // GDA94 / MGA zone 56
    (20010, 28357),  // GDA94 / MGA zone 57
    (20011, 28358),  // GDA94 / MGA zone 58
    (20012, 3112),   // GDA94 / Geoscience Australia Lambert
    (20016, 7849),   // GDA2020 / MGA zone 49
    (20017, 7850),   // GDA2020 / MGA zone 50
    (20018, 7851),   // GDA2020 / MGA zone 51
    (20019, 7852),   // GDA2020 / MGA zone 52
    (20020, 7853),   // GDA2020 / MGA zone 53
    (20021, 7854),   // GDA2020 / MGA zone 54
    (20022, 7855),   // GDA2020 / MGA zone 55
    (20023, 7856),   // GDA2020 / MGA zone 56
    (21001, 23032),  // ED50 / UTM zone 32N
    (21002, 23033),  // ED50 / UTM zone 33N
    (21003, 25832),  // ETRS89 / UTM zone 32N
    (21004, 25833),  // ETRS89 / UTM zone 33N
    (24000, 4272),   // NZGD49
    (26000, 3346),   // LKS94 / Lithuania TM
    (27000, 3301),   // Estonian Coordinate System of 1997
    (28000, 3059),   // LKS92 / Latvia TM
    (29000, 2100),   // GGRS87 / Greek Grid
    (30001, 23028),  // ED50 / UTM zone 28N
    (30002, 23029),  // ED50 / UTM zone 29N
    (30003, 23030),  // ED50 / UTM zone 30N
    (30004, 23030),  // ED50 / UTM zone 30N
    (30005, 23031),  // ED50 / UTM zone 31N
    (30006, 23031),  // ED50 / UTM zone 31N
    (31007, 3765),   // HTRS96 / Croatia TM
    (32000, 23033),  // ED50 / UTM zone 33N
    (33001, 2169),   // Luxembourg 1930 / Gauss
    (33002, 23031),  // ED50 / UTM zone 31N
    (34000, 29902),  // TM65 / Irish Grid
    (35000, 2462),   // Albanian 1987 / Gauss-Kruger zone 4
    (36001, 25833),  // ETRS89 / UTM zone 33N
    (36002, 25834),  // ETRS89 / UTM zone 34N
    (37001, 25834),  // ETRS89 / UTM zone 34N
    (37002, 25835),  // ETRS89 / UTM zone 35N
    (38004, 3057),   // ISN93 / Lambert 1993
    (39000, 23033),  // ED50 / UTM zone 33N
    (40000, 23032),  // ED50 / UTM zone 32N
    (41001, 28991),  // Amersfoort / RD Old
    (41002, 23031),  // ED50 / UTM zone 31N
    (41003, 23032),  // ED50 / UTM zone 32N
    (41004, 28992),  // Amersfoort / RD New
    (42002, 23028),  // ED50 / UTM zone 28N
    (44000, 23033),  // ED50 / UTM zone 33N
    (46001, 5514),   // S-JTSK Krovak East North
    (46002, 28403),  // Pulkovo 1942 / Gauss-Kruger zone 3
    (47001, 5514),   // S-JTSK Krovak East North
    (47002, 28403),  // Pulkovo_1942_GK_Zone_3
    (47003, 28404),  // Pulkovo 1942 / Gauss-Kruger zone 4
    (48001, 2180),   // ETRS89 / Poland CS92
    (48002, 2176),   // ETRS89 / Poland CS2000 zone 5
    (48003, 2177),   // ETRS89 / Poland CS2000 zone 6
    (48004, 2178),   // ETRS89 / Poland CS2000 zone 7
    (48005, 2179),   // ETRS89 / Poland CS2000 zone 8
    (49000, 23700),  // HD72 / EOV
    (50001, 26929),  // NAD83 / Alabama East
    (50002, 26930),  // NAD83 / Alabama West
    (50004, 26932),  // NAD83 / Alaska zone 2
    (50005, 26933),  // NAD83 / Alaska zone 3
    (50006, 26934),  // NAD83 / Alaska zone 4
    (50007, 26935),  // NAD83 / Alaska zone 5
    (50008, 26936),  // NAD83 / Alaska zone 6
    (50009, 26937),  // NAD83 / Alaska zone 7
    (50010, 26938),  // NAD83 / Alaska zone 8
    (50011, 26939),  // NAD83 / Alaska zone 9
    (50012, 26940),  // NAD83 / Alaska zone 10
    (50013, 26948),  // NAD83 / Arizona East
    (50014, 26949),  // NAD83 / Arizona Central
    (50015, 26950),  // NAD83 / Arizona West
    (50016, 26951),  // NAD83 / Arkansas North
    (50017, 26952),  // NAD83 / Arkansas South
    (50018, 26941),  // NAD83 / California zone 1
    (50019, 26942),  // NAD83 / California zone 2
    (50020, 26943),  // NAD83 / California zone 3
    (50021, 26944),  // NAD83 / California zone 4
    (50022, 26945),  // NAD83 / California zone 5
    (50023, 26946),  // NAD83 / California zone 6
    (50024, 26953),  // NAD83 / Colorado North
    (50025, 26954),  // NAD83 / Colorado Central
    (50026, 26955),  // NAD83 / Colorado South
    (50027, 26956),  // NAD83 / Connecticut
    (50028, 26957),  // NAD83 / Delaware
    (50029, 26958),  // NAD83 / Florida East
    (50030, 26959),  // NAD83 / Florida West
    (50031, 26960),  // NAD83 / Florida North
    (50032, 26966),  // NAD83 / Georgia East
    (50033, 26967),  // NAD83 / Georgia West
    (50034, 26961),  // NAD83 / Hawaii zone 1
    (50035, 26962),  // NAD83 / Hawaii zone 2
    (50036, 26963),  // NAD83 / Hawaii zone 3
    (50037, 26964),  // NAD83 / Hawaii zone 4
    (50038, 26965),  // NAD83 / Hawaii zone 5
    (50039, 26968),  // NAD83 / Idaho East
    (50040, 26969),  // NAD83 / Idaho Central
    (50041, 26970),  // NAD83 / Idaho West
    (50042, 26971),  // NAD83 / Illinois East
    (50043, 26972),  // NAD83 / Illinois West
    (50044, 26973),  // NAD83 / Indiana East
    (50045, 26974),  // NAD83 / Indiana West
    (50046, 26975),  // NAD83 / Iowa North
    (50047, 26976),  // NAD83 / Iowa South
    (50048, 26977),  // NAD83 / Kansas North
    (50049, 26978),  // NAD83 / Kansas South
    (50050, 26979),  // NAD_1983_StatePlane_Kentucky_North_FIPS_1601
    (50051, 26980),  // NAD83 / Kentucky South
    (50052, 26981),  // NAD83 / Louisiana North
    (50053, 26982),  // NAD83 / Louisiana South
    (50054, 32199),  // NAD83 / Louisiana Offshore
    (50055, 26983),  // NAD83 / Maine East
    (50056, 26984),  // NAD83 / Maine West
    (50057, 26985),  // NAD83 / Maryland
    (50058, 26986),  // NAD83 / Massachusetts Mainland
    (50059, 26987),  // NAD83 / Massachusetts Island
    (50060, 26988),  // NAD83 / Michigan North
    (50061, 26989),  // NAD83 / Michigan Central
    (50062, 26990),  // NAD83 / Michigan South
    (50063, 26991),  // NAD83 / Minnesota North
    (50064, 26992),  // NAD83 / Minnesota Central
    (50065, 26993),  // NAD83 / Minnesota South
    (50066, 26994),  // NAD83 / Mississippi East
    (50067, 26995),  // NAD83 / Mississippi West
    (50068, 26996),  // NAD83 / Missouri East
    (50069, 26997),  // NAD83 / Missouri Central
    (50070, 26998),  // NAD83 / Missouri West
    (50071, 32100),  // NAD83 / Montana
    (50072, 32104),  // NAD83 / Nebraska
    (50073, 32107),  // NAD83 / Nevada East
    (50074, 32108),  // NAD83 / Nevada Central
    (50075, 32109),  // NAD83 / Nevada West
    (50076, 32110),  // NAD83 / New Hampshire
    (50077, 32111),  // NAD83 / New Jersey
    (50078, 32112),  // NAD83 / New Mexico East
    (50079, 32113),  // NAD83 / New Mexico Central
    (50080, 32114),  // NAD83 / New Mexico West
    (50081, 32115),  // NAD83 / New York East
    (50082, 32116),  // NAD83 / New York Central
    (50083, 32117),  // NAD83 / New York West
    (50084, 32118),  // NAD83 / New York Long Island
    (50085, 32119),  // NAD83 / North Carolina
    (50086, 32120),  // NAD83 / North Dakota North
    (50087, 32121),  // NAD83 / North Dakota South
    (50088, 32122),  // NAD83 / Ohio North
    (50089, 32123),  // NAD83 / Ohio South
    (50090, 32124),  // NAD83 / Oklahoma North
    (50091, 32125),  // NAD83 / Oklahoma South
    (50092, 32126),  // NAD83 / Oregon North
    (50093, 32127),  // NAD83 / Oregon South
    (50094, 32128),  // NAD83 / Pennsylvania North
    (50095, 32129),  // NAD83 / Pennsylvania South
    (50096, 32130),  // NAD83 / Rhode Island
    (50097, 32133),  // NAD83 / South Carolina
    (50098, 32134),  // NAD83 / South Dakota North
    (50099, 32135),  // NAD83 / South Dakota South
    (50100, 32136),  // NAD83 / Tennessee
    (50101, 32137),  // NAD83 / Texas North
    (50102, 32138),  // NAD83 / Texas North Central
    (50103, 32139),  // NAD83 / Texas Central
    (50104, 32140),  // NAD83 / Texas South Central
    (50105, 32141),  // NAD83 / Texas South
    (50106, 32142),  // NAD83 / Utah North
    (50107, 32143),  // NAD83 / Utah Central
    (50108, 32144),  // NAD83 / Utah South
    (50109, 32145),  // NAD83 / Vermont
    (50110, 32146),  // NAD83 / Virginia North
    (50111, 32147),  // NAD83 / Virginia South
    (50112, 32148),  // NAD83 / Washington North
    (50113, 32149),  // NAD83 / Washington South
    (50114, 32150),  // NAD83 / West Virginia North
    (50115, 32151),  // NAD83 / West Virginia South
    (50116, 32152),  // NAD83 / Wisconsin North
    (50117, 32153),  // NAD83 / Wisconsin Central
    (50118, 32154),  // NAD83 / Wisconsin South
    (50119, 32155),  // NAD83 / Wyoming East
    (50120, 32156),  // NAD83 / Wyoming East Central
    (50121, 32157),  // NAD83 / Wyoming West Central
    (50122, 32158),  // NAD83 / Wyoming West
    (50123, 32161),  // NAD83 / Puerto Rico & Virgin Is.
    (52004, 22521),  // Corrego Alegre 1970-72 / UTM zone 21S
    (52005, 22522),  // Corrego Alegre 1970-72 / UTM zone 22S
    (52006, 22523),  // Corrego Alegre 1970-72 / UTM zone 23S
    (52007, 22524),  // Corrego Alegre 1970-72 / UTM zone 24S
    (52008, 22525),  // Corrego Alegre 1970-72 / UTM zone 25S
    (52009, 5875),   // SAD69(96) / UTM zone 18S
    (52010, 5876),   // SAD69(96) / UTM zone 19S
    (52011, 5877),   // SAD69(96) / UTM zone 20S
    (52012, 5531),   // SAD69(96) / UTM zone 21S
    (52013, 5858),   // SAD69(96) / UTM zone 22S
    (52014, 5533),   // SAD69(96) / UTM zone 23S
    (52015, 5534),   // SAD69(96) / UTM zone 24S
    (52016, 5535),   // SAD69(96) / UTM zone 25S
    (52017, 31978),  // SIRGAS 2000 / UTM zone 18S
    (52018, 31979),  // SIRGAS 2000 / UTM zone 19S
    (52019, 31980),  // SIRGAS 2000 / UTM zone 20S
    (52020, 31981),  // SIRGAS 2000 / UTM zone 21S
    (52021, 31982),  // SIRGAS 2000 / UTM zone 22S
    (52022, 31983),  // SIRGAS 2000 / UTM zone 23S
    (52023, 31984),  // SIRGAS 2000 / UTM zone 24S
    (52024, 31985),  // SIRGAS 2000 / UTM zone 25S
    (53000, 2039),   // Israel 1993 / Israeli TM Grid
    (54001, 3375),   // GDM2000 / Peninsula RSO
    (54002, 3376),   // GDM2000 / East Malaysia BRSO
    (56000, 900913), // Google Mercator
    (57000, 2041),   // Abidjan 1987 / UTM zone 30N
    (61000, 2326),   // Hong Kong 1980 Grid System
    (62001, 26901),  // NAD83 / UTM zone 1N
    (62002, 26902),  // NAD83 / UTM zone 2N
    (62004, 26903),  // NAD83 / UTM zone 3N
    (62005, 26904),  // NAD83 / UTM zone 4N
    (62006, 26905),  // NAD83 / UTM zone 5N
    (62007, 26906),  // NAD83 / UTM zone 6N
    (62008, 26907),  // NAD83 / UTM zone 7N
    (62009, 26908),  // NAD83 / UTM zone 8N
    (62010, 26909),  // NAD83 / UTM zone 9N
    (62011, 26910),  // NAD83 / UTM zone 10N
    (62012, 26911),  // NAD83 / UTM zone 11N
    (62013, 26912),  // NAD83 / UTM zone 12N
    (62014, 26913),  // NAD83 / UTM zone 13N
    (62015, 26914),  // NAD83 / UTM zone 14N
    (62016, 26915),  // NAD83 / UTM zone 15N
    (62017, 26916),  // NAD83 / UTM zone 16N
    (62018, 26917),  // NAD83 / UTM zone 17N
    (62019, 26918),  // NAD83 / UTM zone 18N
    (62020, 26919),  // NAD83 / UTM zone 19N
    (62021, 26920),  // NAD83 / UTM zone 20N
    (62022, 26921),  // NAD83 / UTM zone 21N
    (62023, 26922),  // NAD83 / UTM zone 22N
    (62024, 26923),  // NAD83 / UTM zone 23N
    (63001, 25828),  // ETRS89 / UTM zone 28N
    (63002, 25829),  // ETRS89 / UTM zone 29N
    (63003, 25830),  // ETRS89 / UTM zone 30N
    (63004, 25831),  // ETRS89 / UTM zone 31N
    (63005, 25832),  // ETRS89 / UTM zone 32N
    (63006, 25833),  // ETRS89 / UTM zone 33N
    (63007, 25834),  // ETRS89 / UTM zone 34N
    (63008, 25835),  // ETRS89 / UTM zone 35N
    (63009, 25836),  // ETRS89 / UTM zone 36N
    (63010, 25837),  // ETRS89 / UTM zone 37N
    (63011, 25838),  // ETRS_1989_UTM_Zone_38N
    (64000, 3785),   // { OcdGrid::Portugal, 1, 27493 }, // Datum 73 / Modified Portuguese Grid
    (65000, 3857),   // WGS 84 / Pseudo-Mercator
    (66001, 102629), // NAD_1983_StatePlane_Alabama_East_FIPS_0101_Feet
    (66002, 102630), // NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet
    (66013, 2222),   // NAD83 / Arizona East (ft)
    (66014, 2223),   // NAD83 / Arizona Central (ft)
    (66015, 2224),   // NAD83 / Arizona West (ft)
    (66016, 3433),   // NAD83 / Arkansas North (ftUS)
    (66017, 3434),   // NAD83 / Arkansas South (ftUS)
    (66018, 2225),   // NAD83 / California zone 1 (ftUS)
    (66019, 2226),   // NAD83 / California zone 2 (ftUS)
    (66020, 2227),   // NAD83 / California zone 3 (ftUS)
    (66021, 2228),   // NAD83 / California zone 4 (ftUS)
    (66022, 2229),   // NAD83 / California zone 5 (ftUS)
    (66023, 2230),   // NAD83 / California zone 6 (ftUS)
    (66024, 2231),   // NAD83 / Colorado North (ftUS)
    (66025, 2232),   // NAD83 / Colorado Central (ftUS)
    (66026, 2233),   // NAD83 / Colorado South (ftUS)
    (66027, 2234),   // NAD83 / Connecticut (ftUS)
    (66028, 2235),   // NAD83 / Delaware (ftUS)
    (66029, 2236),   // NAD83 / Florida East (ftUS)
    (66030, 2237),   // NAD83 / Florida West (ftUS)
    (66031, 2238),   // NAD83 / Florida North (ftUS)
    (66032, 2239),   // NAD83 / Georgia East (ftUS)
    (66033, 2240),   // NAD83 / Georgia West (ftUS)
    (66036, 3759),   // NAD83 / Hawaii zone 3 (ftUS)
    (66039, 2241),   // NAD83 / Idaho East (ftUS)
    (66040, 2242),   // NAD83 / Idaho Central (ftUS)
    (66041, 2243),   // NAD83 / Idaho West (ftUS)
    (66042, 3435),   // NAD83 / Illinois East (ftUS)
    (66043, 3436),   // NAD83 / Illinois West (ftUS)
    (66044, 2244),   // NAD83 / Indiana East (ftUS) (deprecated record)
    (66045, 2245),   // NAD83 / Indiana West (ftUS) (deprecated record)
    (66046, 3417),   // NAD83 / Iowa North (ftUS)
    (66047, 3418),   // NAD83 / Iowa South (ftUS)
    (66048, 3419),   // NAD83 / Kansas North (ftUS)
    (66049, 3420),   // NAD83 / Kansas South (ftUS)
    (66050, 2246),   // NAD83 / Kentucky North (ftUS)
    (66051, 2247),   // NAD83 / Kentucky South (ftUS)
    (66052, 3451),   // NAD83 / Louisiana North (ftUS)
    (66053, 3452),   // NAD83 / Louisiana South (ftUS)
    (66054, 3453),   // NAD83 / Louisiana Offshore (ftUS)
    (66055, 26847),  // NAD83 / Maine East (ftUS)
    (66056, 26848),  // NAD83 / Maine West (ftUS)
    (66057, 2248),   // NAD83 / Maryland (ftUS)
    (66058, 2249),   // NAD83 / Massachusetts Mainland (ftUS)
    (66059, 2250),   // NAD83 / Massachusetts Island (ftUS)
    (66060, 2251),   // NAD83 / Michigan North (ft)
    (66061, 2252),   // NAD83 / Michigan Central (ft)
    (66062, 2253),   // NAD83 / Michigan South (ft)
    (66063, 26849),  // NAD83 / Minnesota North (ftUS)
    (66064, 26850),  // NAD83 / Minnesota Central (ftUS)
    (66065, 26851),  // NAD83 / Minnesota South (ftUS)
    (66066, 2254),   // NAD83 / Mississippi East (ftUS)
    (66067, 2255),   // NAD83 / Mississippi West (ftUS)
    (66068, 102696), // NAD_1983_StatePlane_Missouri_East_FIPS_2401_Feet
    (66069, 102697), // NAD_1983_StatePlane_Missouri_Central_FIPS_2402_Feet
    (66070, 102698), // NAD_1983_StatePlane_Missouri_West_FIPS_2403_Feet
    (66071, 2256),   // NAD83 / Montana (ft)
    (66072, 26852),  // NAD83 / Nebraska (ftUS)
    (66073, 3421),   // NAD83 / Nevada East (ftUS)
    (66074, 3422),   // NAD83 / Nevada Central (ftUS)
    (66075, 3423),   // NAD83 / Nevada West (ftUS)
    (66076, 3437),   // NAD83 / New Hampshire (ftUS)
    (66077, 3424),   // NAD83 / New Jersey (ftUS)
    (66078, 2257),   // NAD83 / New Mexico East (ftUS)
    (66079, 2258),   // NAD83 / New Mexico Central (ftUS)
    (66080, 2259),   // NAD83 / New Mexico West (ftUS)
    (66081, 2260),   // NAD83 / New York East (ftUS)
    (66082, 2261),   // NAD83 / New York Central (ftUS)
    (66083, 2262),   // NAD83 / New York West (ftUS)
    (66084, 2263),   // NAD83 / New York Long Island (ftUS)
    (66085, 2264),   // NAD83 / North Carolina (ftUS)
    (66086, 2265),   // NAD83 / North Dakota North (ft)
    (66087, 2266),   // NAD83 / North Dakota South (ft)
    (66088, 3734),   // NAD83 / Ohio North (ftUS)
    (66089, 3735),   // NAD83 / Ohio South (ftUS)
    (66090, 2267),   // NAD83 / Oklahoma North (ftUS)
    (66091, 2268),   // NAD83 / Oklahoma South (ftUS)
    (66092, 2269),   // NAD83 / Oregon North (ft)
    (66093, 2270),   // NAD83 / Oregon South (ft)
    (66094, 2271),   // NAD83 / Pennsylvania North (ftUS)
    (66095, 2272),   // NAD83 / Pennsylvania South (ftUS)
    (66096, 3438),   // NAD83 / Rhode Island (ftUS)
    (66097, 2273),   // NAD83 / South Carolina (ft)
    (66098, 3454),   // NAD83 / South Dakota North (ftUS) (deprecated record)
    (66099, 3455),   // NAD83 / South Dakota South (ftUS)
    (66100, 2274),   // NAD83 / Tennessee (ftUS)
    (66101, 2275),   // NAD83 / Texas North (ftUS)
    (66102, 2276),   // NAD83 / Texas North Central (ftUS)
    (66103, 2277),   // NAD83 / Texas Central (ftUS)
    (66104, 2278),   // NAD83 / Texas South Central (ftUS)
    (66105, 2279),   // NAD83 / Texas South (ftUS)
    (66106, 3560),   // NAD83 / Utah North (ftUS)
    (66107, 3566),   // NAD83 / Utah Central (ftUS)
    (66108, 3567),   // NAD83 / Utah South (ftUS)
    (66109, 102745), // NAD_1983_StatePlane_Vermont_FIPS_4400_Feet
    (66110, 2283),   // NAD83 / Virginia North (ftUS)
    (66111, 2284),   // NAD83 / Virginia South (ftUS)
    (66112, 2285),   // NAD83 / Washington North (ftUS)
    (66113, 2286),   // NAD83 / Washington South (ftUS)
    (66114, 26853),  // NAD83 / West Virginia North (ftUS)
    (66115, 26854),  // NAD83 / West Virginia South (ftUS)
    (66116, 2287),   // NAD83 / Wisconsin North (ftUS)
    (66117, 2288),   // NAD83 / Wisconsin Central (ftUS)
    (66118, 2289),   // NAD83 / Wisconsin South (ftUS)
    (66119, 3736),   // NAD83 / Wyoming East (ftUS)
    (66120, 3737),   // NAD83 / Wyoming East Central (ftUS)
    (66121, 3738),   // NAD83 / Wyoming West Central (ftUS)
    (66122, 3739),   // NAD83 / Wyoming West (ftUS)
    (66123, 102761), // NAD_1983_StatePlane_Puerto_Rico_Virgin_Islands_FIPS_5200_Feet
    (68000, 21037),  // Arc 1960 / UTM zone 37S
    (69000, 2586),   // Pulkovo 1942 / 3-degree Gauss-Kruger CM 33E
    (70000, 2019),   // NAD27(76) / MTM zone 10
];

/// The EPSG code for a combined grid and zone code, if the table names one.
pub fn lookup(grid_zone: i32) -> Option<u32> {
    GRID_ZONE_EPSG
        .binary_search_by_key(&grid_zone, |&(code, _)| code)
        .ok()
        .map(|i| GRID_ZONE_EPSG[i].1)
}
