//! The host validates names at every persistence boundary. This is a versioned
//! word policy, not proof of legal identity or a language-complete classifier.
pub const POLICY_VERSION: &str = "household-name-v1";

pub(crate) fn validate(value:&str)->Result<String,String> {
    let name=super::store::clean_text(value,80,false,"display_name")?;
    let key=fold(&name);
    if key.is_empty() {return Err("name_invalid".into());}
    if ROLES.contains(&key.as_str()) {return Err("name_is_role_word".into());}
    // English entries match the whole folded name to avoid rejecting actual
    // names such as Dick or Scunthorpe on an unrelated substring.
    if OFFENSIVE.contains(&key.as_str()) || ["傻逼","傻比","傻屄","煞笔","煞比","沙比","操你","肏你","草泥马","艹泥马","婊子","狗娘养"].iter().any(|word|key.contains(word)) {
        return Err("name_offensive".into());
    }
    Ok(name)
}

fn fold(value:&str)->String {
    value.chars().flat_map(|c| {
        let c=if ('\u{ff01}'..='\u{ff5e}').contains(&c) {char::from_u32(c as u32-0xfee0).unwrap_or(c)} else {c};
        let c=match c {
            'ā'|'á'|'ǎ'|'à'|'Ā'|'Á'|'Ǎ'|'À'=>'a',
            'ē'|'é'|'ě'|'è'|'Ē'|'É'|'Ě'|'È'=>'e',
            'ī'|'í'|'ǐ'|'ì'|'Ī'|'Í'|'Ǐ'|'Ì'=>'i',
            'ō'|'ó'|'ǒ'|'ò'|'Ō'|'Ó'|'Ǒ'|'Ò'=>'o',
            'ū'|'ú'|'ǔ'|'ù'|'ǖ'|'ǘ'|'ǚ'|'ǜ'|'ü'=>'u',
            '媽'=>'妈','爺'=>'爷','親'=>'亲','姊'=>'姐','筆'=>'笔','馬'=>'马',
            other=>other,
        };
        c.to_lowercase()
    }).filter(|c|c.is_alphabetic() && !matches!(*c,'\u{200b}'|'\u{200c}'|'\u{200d}'|'\u{feff}')).collect()
}

const ROLES:&[&str]=&[
    "妈","妈妈","妈咪","爸爸","爸","爹","爹爹","娘","奶奶","爷爷","外婆","外公","姥姥","姥爷",
    "哥哥","姐姐","弟弟","妹妹","宝宝","宝贝","叔叔","阿姨","伯伯","姑姑","舅舅","婶婶",
    "母亲","父亲","兄弟","姐妹","爷","奶","mom","mum","mama","mommy","mummy","mother","dad","daddy","papa","father",
    "grandma","grandpa","grandmother","grandfather","brother","sister","baby","uncle","aunt","auntie",
    "baba","nainai","yeye","waipo","waigong","gege","jiejie","didi","meimei","baobao",
];
const OFFENSIVE:&[&str]=&[
    "fuck","fuckyou","fucker","motherfucker","shit","bitch","asshole","bastard","cunt",
    "shabi","shab","sb","sha逼","傻bi","傻b","caonima","cnm","cao你妈","草你妈","操nima",
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn role_words_and_common_offensive_variants_are_rejected_without_rewriting_real_names() {
        for name in ["妈妈","媽 媽","ＤＡＤ","Mǎ mā","grand-mother","外 婆","傻 B","s h a b i","傻bi","F U C K","操你妈","草泥馬"] {
            assert!(validate(name).is_err(),"{name}");
        }
        for name in ["Jason","小林","张三","José","O'Connor","Anne-Marie","Dick","Scunthorpe","沙博"] {
            assert_eq!(validate(name).unwrap(),name);
        }
    }
}
