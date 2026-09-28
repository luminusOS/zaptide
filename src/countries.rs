//! Countries and their international calling codes, for linking by phone
//! number. Territories that share a code with a larger country list their
//! full prefix, such as Jamaica's 1876.

/// ISO 3166-1 alpha-2 code, English name, and calling code without `+`.
pub struct Country {
    pub iso: &'static str,
    pub name: &'static str,
    pub code: &'static str,
}

macro_rules! countries {
    ($($iso:literal $name:literal $code:literal),* $(,)?) => {
        &[$(Country { iso: $iso, name: $name, code: $code }),*]
    };
}

/// Sorted by name.
pub const COUNTRIES: &[Country] = countries![
    "AF" "Afghanistan" "93",
    "AL" "Albania" "355",
    "DZ" "Algeria" "213",
    "AS" "American Samoa" "1684",
    "AD" "Andorra" "376",
    "AO" "Angola" "244",
    "AI" "Anguilla" "1264",
    "AG" "Antigua and Barbuda" "1268",
    "AR" "Argentina" "54",
    "AM" "Armenia" "374",
    "AW" "Aruba" "297",
    "AU" "Australia" "61",
    "AT" "Austria" "43",
    "AZ" "Azerbaijan" "994",
    "BS" "Bahamas" "1242",
    "BH" "Bahrain" "973",
    "BD" "Bangladesh" "880",
    "BB" "Barbados" "1246",
    "BY" "Belarus" "375",
    "BE" "Belgium" "32",
    "BZ" "Belize" "501",
    "BJ" "Benin" "229",
    "BM" "Bermuda" "1441",
    "BT" "Bhutan" "975",
    "BO" "Bolivia" "591",
    "BA" "Bosnia and Herzegovina" "387",
    "BW" "Botswana" "267",
    "BR" "Brazil" "55",
    "VG" "British Virgin Islands" "1284",
    "BN" "Brunei" "673",
    "BG" "Bulgaria" "359",
    "BF" "Burkina Faso" "226",
    "BI" "Burundi" "257",
    "KH" "Cambodia" "855",
    "CM" "Cameroon" "237",
    "CA" "Canada" "1",
    "CV" "Cape Verde" "238",
    "KY" "Cayman Islands" "1345",
    "CF" "Central African Republic" "236",
    "TD" "Chad" "235",
    "CL" "Chile" "56",
    "CN" "China" "86",
    "CO" "Colombia" "57",
    "KM" "Comoros" "269",
    "CG" "Congo" "242",
    "CD" "Congo (DRC)" "243",
    "CK" "Cook Islands" "682",
    "CR" "Costa Rica" "506",
    "CI" "Côte d’Ivoire" "225",
    "HR" "Croatia" "385",
    "CU" "Cuba" "53",
    "CW" "Curaçao" "599",
    "CY" "Cyprus" "357",
    "CZ" "Czechia" "420",
    "DK" "Denmark" "45",
    "DJ" "Djibouti" "253",
    "DM" "Dominica" "1767",
    "DO" "Dominican Republic" "1809",
    "EC" "Ecuador" "593",
    "EG" "Egypt" "20",
    "SV" "El Salvador" "503",
    "GQ" "Equatorial Guinea" "240",
    "ER" "Eritrea" "291",
    "EE" "Estonia" "372",
    "SZ" "Eswatini" "268",
    "ET" "Ethiopia" "251",
    "FK" "Falkland Islands" "500",
    "FO" "Faroe Islands" "298",
    "FJ" "Fiji" "679",
    "FI" "Finland" "358",
    "FR" "France" "33",
    "GF" "French Guiana" "594",
    "PF" "French Polynesia" "689",
    "GA" "Gabon" "241",
    "GM" "Gambia" "220",
    "GE" "Georgia" "995",
    "DE" "Germany" "49",
    "GH" "Ghana" "233",
    "GI" "Gibraltar" "350",
    "GR" "Greece" "30",
    "GL" "Greenland" "299",
    "GD" "Grenada" "1473",
    "GP" "Guadeloupe" "590",
    "GU" "Guam" "1671",
    "GT" "Guatemala" "502",
    "GN" "Guinea" "224",
    "GW" "Guinea-Bissau" "245",
    "GY" "Guyana" "592",
    "HT" "Haiti" "509",
    "HN" "Honduras" "504",
    "HK" "Hong Kong" "852",
    "HU" "Hungary" "36",
    "IS" "Iceland" "354",
    "IN" "India" "91",
    "ID" "Indonesia" "62",
    "IR" "Iran" "98",
    "IQ" "Iraq" "964",
    "IE" "Ireland" "353",
    "IL" "Israel" "972",
    "IT" "Italy" "39",
    "JM" "Jamaica" "1876",
    "JP" "Japan" "81",
    "JO" "Jordan" "962",
    "KZ" "Kazakhstan" "7",
    "KE" "Kenya" "254",
    "KI" "Kiribati" "686",
    "XK" "Kosovo" "383",
    "KW" "Kuwait" "965",
    "KG" "Kyrgyzstan" "996",
    "LA" "Laos" "856",
    "LV" "Latvia" "371",
    "LB" "Lebanon" "961",
    "LS" "Lesotho" "266",
    "LR" "Liberia" "231",
    "LY" "Libya" "218",
    "LI" "Liechtenstein" "423",
    "LT" "Lithuania" "370",
    "LU" "Luxembourg" "352",
    "MO" "Macao" "853",
    "MG" "Madagascar" "261",
    "MW" "Malawi" "265",
    "MY" "Malaysia" "60",
    "MV" "Maldives" "960",
    "ML" "Mali" "223",
    "MT" "Malta" "356",
    "MH" "Marshall Islands" "692",
    "MQ" "Martinique" "596",
    "MR" "Mauritania" "222",
    "MU" "Mauritius" "230",
    "YT" "Mayotte" "262",
    "MX" "Mexico" "52",
    "FM" "Micronesia" "691",
    "MD" "Moldova" "373",
    "MC" "Monaco" "377",
    "MN" "Mongolia" "976",
    "ME" "Montenegro" "382",
    "MS" "Montserrat" "1664",
    "MA" "Morocco" "212",
    "MZ" "Mozambique" "258",
    "MM" "Myanmar" "95",
    "NA" "Namibia" "264",
    "NR" "Nauru" "674",
    "NP" "Nepal" "977",
    "NL" "Netherlands" "31",
    "NC" "New Caledonia" "687",
    "NZ" "New Zealand" "64",
    "NI" "Nicaragua" "505",
    "NE" "Niger" "227",
    "NG" "Nigeria" "234",
    "KP" "North Korea" "850",
    "MK" "North Macedonia" "389",
    "MP" "Northern Mariana Islands" "1670",
    "NO" "Norway" "47",
    "OM" "Oman" "968",
    "PK" "Pakistan" "92",
    "PW" "Palau" "680",
    "PS" "Palestine" "970",
    "PA" "Panama" "507",
    "PG" "Papua New Guinea" "675",
    "PY" "Paraguay" "595",
    "PE" "Peru" "51",
    "PH" "Philippines" "63",
    "PL" "Poland" "48",
    "PT" "Portugal" "351",
    "PR" "Puerto Rico" "1787",
    "QA" "Qatar" "974",
    "RE" "Réunion" "262",
    "RO" "Romania" "40",
    "RU" "Russia" "7",
    "RW" "Rwanda" "250",
    "KN" "Saint Kitts and Nevis" "1869",
    "LC" "Saint Lucia" "1758",
    "PM" "Saint Pierre and Miquelon" "508",
    "VC" "Saint Vincent and the Grenadines" "1784",
    "WS" "Samoa" "685",
    "SM" "San Marino" "378",
    "ST" "São Tomé and Príncipe" "239",
    "SA" "Saudi Arabia" "966",
    "SN" "Senegal" "221",
    "RS" "Serbia" "381",
    "SC" "Seychelles" "248",
    "SL" "Sierra Leone" "232",
    "SG" "Singapore" "65",
    "SX" "Sint Maarten" "1721",
    "SK" "Slovakia" "421",
    "SI" "Slovenia" "386",
    "SB" "Solomon Islands" "677",
    "SO" "Somalia" "252",
    "ZA" "South Africa" "27",
    "KR" "South Korea" "82",
    "SS" "South Sudan" "211",
    "ES" "Spain" "34",
    "LK" "Sri Lanka" "94",
    "SD" "Sudan" "249",
    "SR" "Suriname" "597",
    "SE" "Sweden" "46",
    "CH" "Switzerland" "41",
    "SY" "Syria" "963",
    "TW" "Taiwan" "886",
    "TJ" "Tajikistan" "992",
    "TZ" "Tanzania" "255",
    "TH" "Thailand" "66",
    "TL" "Timor-Leste" "670",
    "TG" "Togo" "228",
    "TO" "Tonga" "676",
    "TT" "Trinidad and Tobago" "1868",
    "TN" "Tunisia" "216",
    "TR" "Türkiye" "90",
    "TM" "Turkmenistan" "993",
    "TC" "Turks and Caicos Islands" "1649",
    "TV" "Tuvalu" "688",
    "VI" "U.S. Virgin Islands" "1340",
    "UG" "Uganda" "256",
    "UA" "Ukraine" "380",
    "AE" "United Arab Emirates" "971",
    "GB" "United Kingdom" "44",
    "US" "United States" "1",
    "UY" "Uruguay" "598",
    "UZ" "Uzbekistan" "998",
    "VU" "Vanuatu" "678",
    "VA" "Vatican City" "39",
    "VE" "Venezuela" "58",
    "VN" "Vietnam" "84",
    "WF" "Wallis and Futuna" "681",
    "YE" "Yemen" "967",
    "ZM" "Zambia" "260",
    "ZW" "Zimbabwe" "263",
];

impl Country {
    /// The flag emoji, from the code's regional indicator letters.
    pub fn flag(&self) -> String {
        self.iso
            .chars()
            .filter_map(|letter| char::from_u32(0x1F1E6 + (letter as u32 - 'A' as u32)))
            .collect()
    }

    /// "🇧🇷 Brazil +55", as the picker lists it.
    pub fn label(&self) -> String {
        format!("{} {} +{}", self.flag(), self.name, self.code)
    }
}

/// The list position of the country in a locale such as `pt_BR.UTF-8`,
/// else the United States.
pub fn from_locale(locale: &str) -> usize {
    let region = locale
        .split(['.', '@'])
        .next()
        .and_then(|tag| tag.split(['_', '-']).nth(1))
        .map(str::to_ascii_uppercase);
    let position = |iso: &str| COUNTRIES.iter().position(|country| country.iso == iso);
    region
        .as_deref()
        .and_then(position)
        .or_else(|| position("US"))
        .unwrap_or(0)
}

/// The international number for `number` typed with `country` selected.
/// A number starting with `+` already carries its own code and wins.
pub fn international(country: &Country, number: &str) -> String {
    let number = number.trim();
    if number.starts_with('+') {
        number.to_owned()
    } else {
        format!("+{} {number}", country.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries_are_sorted_unique_and_well_formed() {
        // Alphabetical as a reader sees it: accents sort with their letter.
        let fold = |name: &str| -> String {
            name.chars()
                .map(|c| match c {
                    'ã' => 'a',
                    'ç' => 'c',
                    'é' => 'e',
                    'í' => 'i',
                    'ô' => 'o',
                    'ü' => 'u',
                    c => c,
                })
                .collect()
        };
        for pair in COUNTRIES.windows(2) {
            assert!(
                fold(pair[0].name) < fold(pair[1].name),
                "{} before {}",
                pair[0].name,
                pair[1].name
            );
        }
        let mut isos: Vec<_> = COUNTRIES.iter().map(|country| country.iso).collect();
        isos.sort_unstable();
        isos.dedup();
        assert_eq!(isos.len(), COUNTRIES.len());
        for country in COUNTRIES {
            assert!(country.iso.len() == 2 && country.iso.chars().all(|c| c.is_ascii_uppercase()));
            assert!(!country.code.is_empty() && country.code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn locale_picks_the_country_and_numbers_get_its_code() {
        let brazil = &COUNTRIES[from_locale("pt_BR.UTF-8")];
        assert_eq!(brazil.iso, "BR");
        assert_eq!(brazil.flag(), "🇧🇷");
        assert_eq!(COUNTRIES[from_locale("C")].iso, "US");
        assert_eq!(international(brazil, " 11 91234-5678"), "+55 11 91234-5678");
        assert_eq!(
            international(brazil, "+351 912 345 678"),
            "+351 912 345 678"
        );
    }
}
