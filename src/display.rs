pub fn sorted_list<I, T>(list: I) -> impl std::fmt::Display
where
    I: IntoIterator<Item = T>,
    T: Ord + std::fmt::Display,
{
    let mut list = list.into_iter().collect::<Vec<_>>();
    list.sort();
    self::list(list)
}

pub fn list<S, T>(list: S) -> impl std::fmt::Display
where
    S: AsRef<[T]>,
    T: std::fmt::Display,
{
    std::fmt::from_fn(move |f| {
        for (i, item) in list.as_ref().iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{item}")?;
        }
        Ok(())
    })
}
