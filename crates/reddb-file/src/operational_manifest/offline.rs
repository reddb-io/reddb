use super::*;

impl<V: Vfs> OperationalManifest<V> {
    /// Qualify the marker-only layout for a read-only recovery copy. Segment,
    /// shared-fork and pending-operation recovery require separate handling.
    pub fn verify_marker_only_for_recovery(&self) -> io::Result<()> {
        if self.vfs.exists(&self.root.join(NEXT_MANIFEST_FILE)) {
            return Err(invalid_data("pending operational manifest"));
        }
        let manifest = self
            .load_current()?
            .ok_or_else(|| invalid_data("missing operational manifest"))?;
        if manifest.fork_origin.is_some() || !manifest.append_only_segments.is_empty() {
            return Err(invalid_data(
                "operational segments or forks require separate recovery",
            ));
        }
        for entry in manifest.collections.values() {
            if entry.state != CollectionState::Active || entry.source.is_some() {
                return Err(invalid_data("pending or shared operational collection"));
            }
            let mut components = Path::new(&entry.path).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                || components.next().is_some()
            {
                return Err(invalid_data("invalid operational collection path"));
            }
            let path = self.collections_dir().join(&entry.path);
            let mut file = self.vfs.open(&path, OpenMode::read_only())?;
            if file.read(&mut [0u8; 1])? != 0 {
                return Err(invalid_data(
                    "nonempty operational collection requires separate recovery",
                ));
            }
        }
        Ok(())
    }
}
