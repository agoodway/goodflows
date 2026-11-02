# Spec Delta

## REMOVED Requirements

### Requirement: Partitions are not combined with reporting
**Reason**: Per-instance report collection now gives each instance its own directory and partition metadata.
**Migration**: Partitioned jobs may declare `reports = ["exunit-timings"]`. Collection behavior is specified by `test-timing-publishing`.
