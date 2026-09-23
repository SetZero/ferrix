macro_rules! many {
    ($($name:ident),*) => {
        $(
            pub fn $name(x: u64) -> String {
                let mut v: Vec<u64> = (0..x).map(|i| i.wrapping_mul(2654435761)).collect();
                v.sort_unstable();
                format!("{}: {:?}", stringify!($name), &v[..v.len().min(8)])
            }
        )*
    };
}

many!(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9, b0, b1, b2, b3, b4, b5, b6, b7, b8, b9,
      c0, c1, c2, c3, c4, c5, c6, c7, c8, c9, d0, d1, d2, d3, d4, d5, d6, d7, d8, d9);
