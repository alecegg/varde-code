use b::fn_b;
use c::fn_c;
use missing::thing;

fn main() {
    fn_b();
    fn_c();
    ghost();
}
