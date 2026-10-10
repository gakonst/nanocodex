-- 0010's AFTER INSERT projections selected new links from crm_graph_source_links
-- filtered by computed node ids. That view unions every legacy table, so each
-- inserted record, meeting or attendee rescanned all of its owner's CRM rows and
-- a dense Calendar backfill grew quadratically. These triggers project the same
-- links: only the crm_graph_source_links branches whose source or target can be
-- the new row's node, each restricted by indexed key columns instead.
CREATE INDEX crm_interactions_meeting ON crm_interactions(owner_id,meeting_id);
CREATE INDEX crm_email_imports_record ON crm_email_imports(owner_id,record_id);

DROP TRIGGER crm_graph_crm_records_insert;
CREATE TRIGGER crm_graph_crm_records_insert AFTER INSERT ON crm_records
BEGIN
 INSERT INTO crm_nodes SELECT * FROM crm_graph_crm_records WHERE owner_id=NEW.owner_id AND id='legacy:crm_records:' || json_array(NEW.id)
 ON CONFLICT(owner_id,id) DO UPDATE SET text=excluded.text,metadata=excluded.metadata,
  created_at=excluded.created_at,updated_at=excluded.updated_at;
 INSERT INTO crm_links
 SELECT l.owner_id,min(l.source_id,l.target_id),max(l.source_id,l.target_id),l.created_at,l.updated_at FROM (
 SELECT * FROM (
 SELECT s.owner_id, 'legacy:crm_records:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.company_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_records s WHERE s.owner_id=NEW.owner_id AND (s.id=NEW.id OR s.company_id=NEW.id)
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_notes:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_notes s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_facts:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_facts s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_identities:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_identities s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id)
 UNION ALL
 SELECT * FROM (
 SELECT s.owner_id, 'legacy:crm_research:' || json_array(s.record_id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.checked_at AS created_at, s.checked_at AS updated_at FROM crm_research s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_relationships:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.from_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_relationships s WHERE s.owner_id=NEW.owner_id AND s.from_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_relationships:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.to_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_relationships s WHERE s.owner_id=NEW.owner_id AND s.to_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_event_participation:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_event_participation s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id)
 UNION ALL
 SELECT * FROM (
 SELECT s.owner_id, 'legacy:crm_interactions:' || json_array(s.id) AS source_id, 'legacy:crm_records:' || json_array(s.person_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_interactions s WHERE s.owner_id=NEW.owner_id AND s.person_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_interaction_participants:' || json_array(s.interaction_id, s.record_id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, (SELECT p.created_at FROM crm_interactions p WHERE p.owner_id=s.owner_id AND p.id=s.interaction_id) AS created_at, (SELECT p.updated_at FROM crm_interactions p WHERE p.owner_id=s.owner_id AND p.id=s.interaction_id) AS updated_at FROM crm_interaction_participants s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_meeting_attendees:' || json_array(s.meeting_id, s.ordinal) AS source_id, 'legacy:crm_records:' || json_array(s.person_id) AS target_id, (SELECT p.created_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS created_at, (SELECT p.updated_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS updated_at FROM crm_meeting_attendees s WHERE s.owner_id=NEW.owner_id AND s.person_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_email_imports:' || json_array(s.connection_id, s.message_id) AS source_id, 'legacy:crm_records:' || json_array(s.record_id) AS target_id, s.imported_at AS created_at, s.imported_at AS updated_at FROM crm_email_imports s WHERE s.owner_id=NEW.owner_id AND s.record_id=NEW.id)
 ) l
 WHERE l.source_id != l.target_id
  AND EXISTS (SELECT 1 FROM crm_nodes a WHERE a.owner_id=l.owner_id AND a.id=l.source_id)
  AND EXISTS (SELECT 1 FROM crm_nodes b WHERE b.owner_id=l.owner_id AND b.id=l.target_id)
 ON CONFLICT(owner_id,from_id,to_id) DO NOTHING;
END;

DROP TRIGGER crm_graph_crm_meetings_insert;
CREATE TRIGGER crm_graph_crm_meetings_insert AFTER INSERT ON crm_meetings
BEGIN
 INSERT INTO crm_nodes SELECT * FROM crm_graph_crm_meetings WHERE owner_id=NEW.owner_id AND id='legacy:crm_meetings:' || json_array(NEW.id)
 ON CONFLICT(owner_id,id) DO UPDATE SET text=excluded.text,metadata=excluded.metadata,
  created_at=excluded.created_at,updated_at=excluded.updated_at;
 INSERT INTO crm_links
 SELECT l.owner_id,min(l.source_id,l.target_id),max(l.source_id,l.target_id),l.created_at,l.updated_at FROM (
 SELECT s.owner_id, 'legacy:crm_interactions:' || json_array(s.id) AS source_id, 'legacy:crm_meetings:' || json_array(s.meeting_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_interactions s WHERE s.owner_id=NEW.owner_id AND s.meeting_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_meeting_attendees:' || json_array(s.meeting_id, s.ordinal) AS source_id, 'legacy:crm_meetings:' || json_array(s.meeting_id) AS target_id, (SELECT p.created_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS created_at, (SELECT p.updated_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS updated_at FROM crm_meeting_attendees s WHERE s.owner_id=NEW.owner_id AND s.meeting_id=NEW.id
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_meeting_notes:' || json_array(s.id) AS source_id, 'legacy:crm_meetings:' || json_array(s.meeting_id) AS target_id, s.created_at AS created_at, s.updated_at AS updated_at FROM crm_meeting_notes s WHERE s.owner_id=NEW.owner_id AND s.meeting_id=NEW.id
 ) l
 WHERE l.source_id != l.target_id
  AND EXISTS (SELECT 1 FROM crm_nodes a WHERE a.owner_id=l.owner_id AND a.id=l.source_id)
  AND EXISTS (SELECT 1 FROM crm_nodes b WHERE b.owner_id=l.owner_id AND b.id=l.target_id)
 ON CONFLICT(owner_id,from_id,to_id) DO NOTHING;
END;

DROP TRIGGER crm_graph_crm_meeting_attendees_insert;
CREATE TRIGGER crm_graph_crm_meeting_attendees_insert AFTER INSERT ON crm_meeting_attendees
BEGIN
 INSERT INTO crm_nodes SELECT * FROM crm_graph_crm_meeting_attendees WHERE owner_id=NEW.owner_id AND id='legacy:crm_meeting_attendees:' || json_array(NEW.meeting_id, NEW.ordinal)
 ON CONFLICT(owner_id,id) DO UPDATE SET text=excluded.text,metadata=excluded.metadata,
  created_at=excluded.created_at,updated_at=excluded.updated_at;
 INSERT INTO crm_links
 SELECT l.owner_id,min(l.source_id,l.target_id),max(l.source_id,l.target_id),l.created_at,l.updated_at FROM (
 SELECT s.owner_id, 'legacy:crm_meeting_attendees:' || json_array(s.meeting_id, s.ordinal) AS source_id, 'legacy:crm_meetings:' || json_array(s.meeting_id) AS target_id, (SELECT p.created_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS created_at, (SELECT p.updated_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS updated_at FROM crm_meeting_attendees s WHERE s.owner_id=NEW.owner_id AND s.meeting_id=NEW.meeting_id AND s.ordinal=NEW.ordinal
 UNION ALL
 SELECT s.owner_id, 'legacy:crm_meeting_attendees:' || json_array(s.meeting_id, s.ordinal) AS source_id, 'legacy:crm_records:' || json_array(s.person_id) AS target_id, (SELECT p.created_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS created_at, (SELECT p.updated_at FROM crm_meetings p WHERE p.owner_id=s.owner_id AND p.id=s.meeting_id) AS updated_at FROM crm_meeting_attendees s WHERE s.owner_id=NEW.owner_id AND s.meeting_id=NEW.meeting_id AND s.ordinal=NEW.ordinal
 ) l
 WHERE l.source_id != l.target_id
  AND EXISTS (SELECT 1 FROM crm_nodes a WHERE a.owner_id=l.owner_id AND a.id=l.source_id)
  AND EXISTS (SELECT 1 FROM crm_nodes b WHERE b.owner_id=l.owner_id AND b.id=l.target_id)
 ON CONFLICT(owner_id,from_id,to_id) DO NOTHING;
END;
