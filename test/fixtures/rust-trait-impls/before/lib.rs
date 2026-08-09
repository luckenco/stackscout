struct Thing;
trait First {
    fn work(&self);
}
trait Second {
    fn work(&self);
}

impl First for Thing {
    fn work(&self) {
        let _ = 1;
    }
}

impl Second for Thing {
    fn work(&self) {}
}
