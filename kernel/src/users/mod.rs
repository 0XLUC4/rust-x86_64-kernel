// =============================================================================
// users — comptes utilisateurs locaux.
//
// Source : fichier texte `/etc/passwd` dans le ramfs, une ligne par compte :
//
//     <username>:<sha256_hex_password>:<uid>:<gid>:<gecos>:<home>:<shell>
//
// Exemple :
//     root:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855:0:0:root:/root:/bin/sh
//
// Note : on utilise le même fichier que `/etc/passwd` UNIX avec le hash **dans**
// le champ password (pas de shadow séparé). C'est volontaire pour garder la
// simplicité ; aucun process user non-root ne doit lire /etc/passwd (à enforcer
// plus tard via permissions FS).
//
// Convention hash : SHA-256(salt || password) avec salt vide pour l'instant.
// Un champ `salt` pourra être ajouté plus tard (format `$6$salt$hash`).
// =============================================================================

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::crypto::sha256_hex;
use crate::task::process::{Uid, Gid};

pub const PASSWD_PATH: &str = "/etc/passwd";

#[derive(Debug, Clone)]
pub struct User {
    pub name: String,
    pub password_hash: String,
    pub uid: Uid,
    pub gid: Gid,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

/// Parse le contenu de `/etc/passwd` (UTF-8, lignes séparées par `\n`).
/// Les lignes vides et commençant par `#` sont ignorées.
pub fn parse_passwd(content: &str) -> Vec<User> {
    let mut users = Vec::new();
    for line in content.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') { continue; }
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 7 { continue; }
        let uid = match parts[2].parse::<u32>() { Ok(v) => v, Err(_) => continue };
        let gid = match parts[3].parse::<u32>() { Ok(v) => v, Err(_) => continue };
        users.push(User {
            name: parts[0].to_string(),
            password_hash: parts[1].to_string(),
            uid, gid,
            gecos: parts[4].to_string(),
            home: parts[5].to_string(),
            shell: parts[6].to_string(),
        });
    }
    users
}

/// Lit `/etc/passwd` depuis le ramfs.
pub fn load() -> Result<Vec<User>, &'static str> {
    let fs = crate::fs::FS.lock();
    let bytes = fs.read(PASSWD_PATH).map_err(|_| "users: /etc/passwd introuvable")?;
    let s = core::str::from_utf8(&bytes).map_err(|_| "users: /etc/passwd non-UTF8")?;
    Ok(parse_passwd(s))
}

/// Retrouve un user par nom.
pub fn find(name: &str) -> Option<User> {
    load().ok()?.into_iter().find(|u| u.name == name)
}

/// Vérifie un mot de passe en clair contre le hash stocké.
/// Retourne Some(user) si OK, None sinon. Utilise une comparaison à temps
/// constant pour résister aux timing attacks simples.
pub fn authenticate(name: &str, password: &str) -> Option<User> {
    let user = find(name)?;
    let computed = sha256_hex(password.as_bytes());
    if constant_time_eq(computed.as_bytes(), user.password_hash.as_bytes()) {
        Some(user)
    } else {
        None
    }
}

/// Comparaison à temps constant (pour strings de même longueur ; si longueurs
/// différentes → false direct, mais on parcourt quand même la plus courte).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Calcule le hash d'un mot de passe clair — utilisé par useradd/passwd.
pub fn hash_password(password: &str) -> String {
    sha256_hex(password.as_bytes())
}

/// Sérialise une liste d'utilisateurs au format `/etc/passwd`.
fn serialize(users: &[User]) -> String {
    let mut out = String::new();
    for u in users {
        out.push_str(&alloc::format!(
            "{}:{}:{}:{}:{}:{}:{}\n",
            u.name, u.password_hash, u.uid, u.gid,
            u.gecos, u.home, u.shell,
        ));
    }
    out
}

/// Écrit la liste dans `/etc/passwd` + persiste sur disque.
fn save(users: &[User]) -> Result<(), &'static str> {
    let content = serialize(users);
    {
        let mut fs = crate::fs::FS.lock();
        fs.write(PASSWD_PATH, content.as_bytes());
    }
    crate::persist::save_from_ramfs()
}

/// Erreurs possibles lors de la création d'un compte.
#[derive(Debug)]
pub enum UserError {
    NameTaken,
    NameInvalid,
    PasswordTooShort,
    SaveFailed(&'static str),
}

impl UserError {
    pub fn message(&self) -> &'static str {
        match self {
            UserError::NameTaken => "nom d'utilisateur déjà pris",
            UserError::NameInvalid => "nom invalide (lettres/chiffres uniquement, 1..32)",
            UserError::PasswordTooShort => "mot de passe trop court (minimum 4 caractères)",
            UserError::SaveFailed(e) => e,
        }
    }
}

fn name_valid(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 { return false; }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Crée un nouveau compte utilisateur avec mot de passe.
/// Le nouvel uid est pris comme max(existants) + 1, min 1000.
pub fn create_user(name: &str, password: &str) -> Result<User, UserError> {
    if !name_valid(name) { return Err(UserError::NameInvalid); }
    if password.len() < 4 { return Err(UserError::PasswordTooShort); }

    let mut users = load().unwrap_or_default();
    if users.iter().any(|u| u.name == name) {
        return Err(UserError::NameTaken);
    }

    let next_uid = users.iter()
        .filter(|u| u.uid >= 1000)
        .map(|u| u.uid)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1000);

    let new_user = User {
        name: name.to_string(),
        password_hash: hash_password(password),
        uid: next_uid,
        gid: next_uid,
        gecos: name.to_string(),
        home: alloc::format!("/home/{}", name),
        shell: String::from("/bin/sh"),
    };
    users.push(new_user.clone());
    save(&users).map_err(UserError::SaveFailed)?;
    Ok(new_user)
}

/// Change le mot de passe d'un utilisateur (identifié par son uid).
pub fn set_password(uid: Uid, new_password: &str) -> Result<(), UserError> {
    if new_password.len() < 4 { return Err(UserError::PasswordTooShort); }
    let mut users = load().unwrap_or_default();
    let u = users.iter_mut().find(|u| u.uid == uid).ok_or(UserError::NameInvalid)?;
    u.password_hash = hash_password(new_password);
    save(&users).map_err(UserError::SaveFailed)
}

/// Supprime un compte utilisateur. Ne peut pas supprimer root (uid=0).
pub fn delete_user(uid: Uid) -> Result<(), UserError> {
    if uid == 0 { return Err(UserError::NameInvalid); }
    let mut users = load().unwrap_or_default();
    let before = users.len();
    users.retain(|u| u.uid != uid);
    if users.len() == before { return Err(UserError::NameInvalid); }
    save(&users).map_err(UserError::SaveFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let content = "root:abc:0:0:root:/root:/bin/sh\nluc:def:1000:1000:Luc:/home/luc:/bin/sh\n";
        let users = parse_passwd(content);
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].name, "root");
        assert_eq!(users[0].uid, 0);
        assert_eq!(users[1].name, "luc");
        assert_eq!(users[1].uid, 1000);
    }

    #[test]
    fn parse_skips_invalid() {
        let content = "# commentaire\n\nmalformé\nroot:abc:0:0:root:/root:/bin/sh\n";
        assert_eq!(parse_passwd(content).len(), 1);
    }
}
