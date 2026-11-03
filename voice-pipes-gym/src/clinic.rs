//! Native in-memory clinic booking & appointment scheduling task environment.
//!
//! Models doctor schedules, slot availability, and patient reservations
//! for conversational multi-turn schedule negotiation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use voice_pipes_sdk::agent::{ClientToolCall, ClientToolSpec};

use crate::env::{TaskEnvironment, TaskEvaluation};

/// Record of a confirmed patient appointment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppointmentBooking {
    pub booking_id: String,
    pub doctor: String,
    pub date: String,
    pub time: String,
    pub patient_name: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ClinicSnapshot {
    available_slots: BTreeMap<String, BTreeSet<String>>,
    bookings: BTreeMap<String, AppointmentBooking>,
    next_id: usize,
}

/// Verifiable goal for clinic scheduling scenarios.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ClinicGoal {
    /// An appointment was booked for the given patient, doctor, and slot.
    AppointmentBooked {
        doctor: String,
        date: String,
        time: String,
        patient_name: String,
    },
    /// Multiple required appointment goals.
    AllOf(Vec<ClinicGoal>),
}

/// In-memory clinic scheduling environment.
#[derive(Debug, Clone)]
pub struct ClinicBookingTaskEnv {
    /// Map of "Doctor|Date" -> Set of available times (e.g. "Dr. Smith|2026-11-04" -> {"10:00", "14:00"})
    available_slots: BTreeMap<String, BTreeSet<String>>,
    bookings: BTreeMap<String, AppointmentBooking>,
    next_id: usize,
    snapshot: ClinicSnapshot,
}

impl Default for ClinicBookingTaskEnv {
    fn default() -> Self {
        Self::new_default()
    }
}

impl ClinicBookingTaskEnv {
    /// Create standard clinic scenario with doctors and slot availability.
    pub fn new_default() -> Self {
        let mut available_slots = BTreeMap::new();

        let mut smith_slots = BTreeSet::new();
        smith_slots.insert("10:00".to_string());
        smith_slots.insert("14:00".to_string());
        available_slots.insert("Dr. Smith|2026-11-04".to_string(), smith_slots);

        let mut taylor_slots = BTreeSet::new();
        taylor_slots.insert("09:00".to_string());
        taylor_slots.insert("11:30".to_string());
        taylor_slots.insert("15:00".to_string());
        available_slots.insert("Dr. Taylor|2026-11-04".to_string(), taylor_slots);

        let snapshot = ClinicSnapshot {
            available_slots: available_slots.clone(),
            bookings: BTreeMap::new(),
            next_id: 1,
        };

        Self {
            available_slots,
            bookings: BTreeMap::new(),
            next_id: 1,
            snapshot,
        }
    }

    /// Slot key helper.
    fn slot_key(doctor: &str, date: &str) -> String {
        format!("{doctor}|{date}")
    }

    /// Evaluate current in-memory bookings against scenario goal.
    pub fn verify_goal(&self, goal: &ClinicGoal) -> TaskEvaluation {
        match goal {
            ClinicGoal::AppointmentBooked {
                doctor,
                date,
                time,
                patient_name,
            } => {
                let matching = self.bookings.values().find(|b| {
                    b.doctor.eq_ignore_ascii_case(doctor)
                        && b.date == *date
                        && b.time == *time
                        && b.patient_name.eq_ignore_ascii_case(patient_name)
                });

                if let Some(booking) = matching {
                    let id = &booking.booking_id;
                    let p_name = &booking.patient_name;
                    let doc = &booking.doctor;
                    let d = &booking.date;
                    let t = &booking.time;
                    TaskEvaluation::success(format!(
                        "Appointment #{id} confirmed: {p_name} with {doc} on {d} at {t}"
                    ))
                } else if self.bookings.is_empty() {
                    TaskEvaluation::failure("No appointment was booked by the agent")
                } else {
                    TaskEvaluation::failure(format!(
                        "Booking mismatch: expected {patient_name} with {doctor} on {date} at {time}"
                    ))
                }
            }

            ClinicGoal::AllOf(subgoals) => {
                let mut total_score = 0.0;
                let mut messages = Vec::new();
                let count = subgoals.len();
                if count == 0 {
                    return TaskEvaluation::success("Empty goal satisfied");
                }
                for sub in subgoals {
                    let eval = self.verify_goal(sub);
                    total_score += eval.score;
                    messages.push(eval.message);
                }
                let final_score = total_score / (count as f32);
                TaskEvaluation {
                    success: final_score >= 0.99,
                    score: final_score,
                    message: messages.join("; "),
                }
            }
        }
    }

    fn execute_find_slots(&self, args: &Value) -> Result<Value, String> {
        let doctor = args
            .get("doctor")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'doctor'".to_string())?;

        let date = args
            .get("date")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'date'".to_string())?;

        let key = Self::slot_key(doctor, date);
        let slots = self.available_slots.get(&key);
        let times: Vec<String> = match slots {
            Some(s) => s.iter().cloned().collect(),
            None => Vec::new(),
        };

        Ok(json!({
            "doctor": doctor,
            "date": date,
            "available_times": times,
            "count": times.len()
        }))
    }

    fn execute_book(&mut self, args: &Value) -> Result<Value, String> {
        let doctor = args
            .get("doctor")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'doctor'".to_string())?;

        let date = args
            .get("date")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'date'".to_string())?;

        let time = args
            .get("time")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'time'".to_string())?;

        let patient = args
            .get("patient_name")
            .and_then(Value::as_str)
            .ok_or_else(|| "Missing required parameter 'patient_name'".to_string())?;

        let key = Self::slot_key(doctor, date);
        let slots = self
            .available_slots
            .get_mut(&key)
            .ok_or_else(|| format!("No clinic schedule found for {doctor} on {date}"))?;

        if !slots.remove(time) {
            return Err(format!(
                "Time slot '{time}' is not available for {doctor} on {date}"
            ));
        }

        let booking_id = format!("APT-{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);

        let booking = AppointmentBooking {
            booking_id: booking_id.clone(),
            doctor: doctor.to_string(),
            date: date.to_string(),
            time: time.to_string(),
            patient_name: patient.to_string(),
        };

        self.bookings.insert(booking_id.clone(), booking);

        Ok(json!({
            "status": "confirmed",
            "booking_id": booking_id,
            "doctor": doctor,
            "date": date,
            "time": time,
            "patient_name": patient
        }))
    }
}

impl TaskEnvironment for ClinicBookingTaskEnv {
    fn name(&self) -> &'static str {
        "clinic_booking"
    }

    fn tools(&self) -> Vec<ClientToolSpec> {
        vec![
            ClientToolSpec::new(
                "find_slots",
                "Find available appointment time slots for a specific doctor and date.",
                json!({
                    "type": "object",
                    "properties": {
                        "doctor": {
                            "type": "string",
                            "description": "Doctor's name (e.g. 'Dr. Smith' or 'Dr. Taylor')"
                        },
                        "date": {
                            "type": "string",
                            "description": "Date in YYYY-MM-DD format (e.g. '2026-11-04')"
                        }
                    },
                    "required": ["doctor", "date"]
                }),
            ),
            ClientToolSpec::new(
                "book_appointment",
                "Reserve a confirmed appointment slot for a patient with a doctor.",
                json!({
                    "type": "object",
                    "properties": {
                        "doctor": { "type": "string" },
                        "date": { "type": "string" },
                        "time": { "type": "string", "description": "Time in HH:MM format (e.g. '14:00')" },
                        "patient_name": { "type": "string" }
                    },
                    "required": ["doctor", "date", "time", "patient_name"]
                }),
            ),
        ]
    }

    fn execute(&mut self, call: &ClientToolCall) -> Result<Value, String> {
        match call.name.as_str() {
            "find_slots" => self.execute_find_slots(&call.arguments),
            "book_appointment" => self.execute_book(&call.arguments),
            unknown => Err(format!("Unknown clinic tool '{unknown}'")),
        }
    }

    fn reset(&mut self) {
        self.available_slots = self.snapshot.available_slots.clone();
        self.bookings.clear();
        self.next_id = 1;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_and_book_slot() {
        let mut env = ClinicBookingTaskEnv::new_default();

        let find = ClientToolCall::new(
            "c1",
            "find_slots",
            json!({"doctor": "Dr. Smith", "date": "2026-11-04"}),
        );
        let res = env.execute(&find).expect("find slots");
        assert_eq!(res["count"], 2);

        let book = ClientToolCall::new(
            "c2",
            "book_appointment",
            json!({
                "doctor": "Dr. Smith",
                "date": "2026-11-04",
                "time": "14:00",
                "patient_name": "Carol Danvers"
            }),
        );
        let book_res = env.execute(&book).expect("book slot");
        assert_eq!(book_res["status"], "confirmed");

        let goal = ClinicGoal::AppointmentBooked {
            doctor: "Dr. Smith".to_string(),
            date: "2026-11-04".to_string(),
            time: "14:00".to_string(),
            patient_name: "Carol Danvers".to_string(),
        };
        let eval = env.verify_goal(&goal);
        assert!(eval.success);
    }

    #[test]
    fn test_cannot_double_book_same_slot() {
        let mut env = ClinicBookingTaskEnv::new_default();

        let book1 = ClientToolCall::new(
            "c1",
            "book_appointment",
            json!({
                "doctor": "Dr. Smith",
                "date": "2026-11-04",
                "time": "10:00",
                "patient_name": "Patient A"
            }),
        );
        env.execute(&book1).expect("book 1");

        let book2 = ClientToolCall::new(
            "c2",
            "book_appointment",
            json!({
                "doctor": "Dr. Smith",
                "date": "2026-11-04",
                "time": "10:00",
                "patient_name": "Patient B"
            }),
        );
        let err = env.execute(&book2).unwrap_err();
        assert!(err.contains("not available"));
    }
}
