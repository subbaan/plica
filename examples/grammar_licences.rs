//! Prints the licences of the syntax grammars compiled into Plica (from
//! two-face), for THIRD_PARTY_LICENSES.md. `make notices` runs it.
//! Output: one "@@@ path" line per grammar licence, then its text.

fn main() {
    for l in two_face::acknowledgement::listing().for_syntaxes() {
        println!("@@@ {}", l.rel_path.display());
        println!("{}", l.text.trim());
    }
}
