//! Intègre le logo dans l'exe (ressource 1 : icône de l'Explorateur, et de la zone de notification).

fn main() {
    println!("cargo:rerun-if-changed=assets/lense.rc");
    println!("cargo:rerun-if-changed=assets/lense.ico");
    let obj = format!("{}/lense-res.o", std::env::var("OUT_DIR").unwrap());
    // shortcut: windres vient de la chaîne MinGW (cible GNU) ; avec MSVC il faudrait rc.exe
    let ok = std::process::Command::new("windres")
        .args(["assets/lense.rc", "-O", "coff", "-o", &obj])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={obj}");
    } else {
        println!("cargo:warning=windres introuvable : l'exe n'aura pas d'icône");
    }
}
