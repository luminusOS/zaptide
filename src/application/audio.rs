use super::*;

impl NativeApplication {
    pub(super) fn voice_send_pending(&self) -> bool {
        self.audio.voice_send_pending
    }

    pub(super) fn audio_registry(&self) -> AudioRegistry {
        self.audio.audio_registry.clone()
    }

    pub(super) fn clear_missing_selected_voice(&mut self) {
        if self
            .audio
            .selected_voice_message
            .as_ref()
            .is_some_and(|id| !self.message_snapshots.contains_key(id))
        {
            self.audio.selected_voice = None;
            self.audio.selected_voice_message = None;
        }
    }

    fn update_audio_row(&mut self, id: &str) {
        let Some(voice) = self
            .message_snapshots
            .get(id)
            .and_then(|message| self.project_voice(message))
        else {
            return;
        };
        if let Some(position) = self.message_ids.iter().position(|known| known == id)
            && let Some(row) = self.messages.get(position as u32)
        {
            row.borrow_mut().audio = Some(voice.clone());
        }
        if let Some(controls) = self.audio.audio_registry.borrow().get(id) {
            controls.update(&voice);
        }
    }

    pub(super) fn queue_waveforms(&mut self) {
        let Some(chat) = &self.active_chat else {
            return;
        };
        for id in &self.message_ids {
            let Some(message) = self.message_snapshots.get(id) else {
                continue;
            };
            let crate::model::Content::Audio {
                media, waveform, ..
            } = &message.content
            else {
                continue;
            };
            let Some(path) = &media.path else { continue };
            if !waveform.is_empty()
                || self
                    .audio
                    .audio_waveforms
                    .contains_key(&(chat.clone(), id.clone()))
                || self
                    .audio
                    .waveform_attempted
                    .contains(&(chat.clone(), id.clone()))
                || self
                    .audio
                    .waveform_queue
                    .iter()
                    .any(|(queued_chat, queued_id, _)| queued_chat == chat && queued_id == id)
                || !path.is_file()
            {
                continue;
            }
            self.audio
                .waveform_queue
                .push_back((chat.clone(), id.clone(), path.clone()));
        }
        self.pump_waveforms(&self.pointer_sender.clone());
    }

    fn pump_waveforms(&mut self, sender: &ComponentSender<Self>) {
        if self.audio.waveform_busy {
            return;
        }
        while let Some((chat, id, path)) = self.audio.waveform_queue.pop_front() {
            if self.active_chat.as_deref() != Some(&chat)
                || self
                    .audio
                    .audio_waveforms
                    .contains_key(&(chat.clone(), id.clone()))
            {
                continue;
            }
            let sender = sender.clone();
            self.audio.waveform_busy = true;
            self.audio
                .waveform_attempted
                .insert((chat.clone(), id.clone()));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.audio.waveform_cancel = Some(cancel.clone());
            if std::thread::Builder::new()
                .name("audio-waveform".into())
                .spawn(move || {
                    let bars = crate::audio::waveform_file_cancellable(&path, &cancel).ok();
                    sender.input(Input::AudioWaveformReady { chat, id, bars });
                })
                .is_err()
            {
                self.audio.waveform_busy = false;
                self.audio.waveform_cancel = None;
            }
            break;
        }
    }

    pub(super) fn waveform_ready(
        &mut self,
        chat: String,
        id: String,
        bars: Option<Vec<u8>>,
        sender: &ComponentSender<Self>,
    ) {
        self.audio.waveform_busy = false;
        self.audio.waveform_cancel = None;
        if self.active_chat.as_deref() == Some(&chat)
            && let Some(bars) = bars
        {
            self.audio.audio_waveforms.insert((chat, id.clone()), bars);
            self.refresh_message_row(&id);
        }
        self.pump_waveforms(sender);
    }

    pub(super) fn audio_control(
        &mut self,
        id: &str,
        intent: crate::native_voice::VoiceIntent,
        sender: &ComponentSender<Self>,
    ) {
        let Some(audio_message) = self
            .message_snapshots
            .get(id)
            .cloned()
            .filter(|message| self.active_chat.as_deref() == Some(&message.chat))
        else {
            return;
        };
        let Some(voice) = self.project_voice(&audio_message) else {
            return;
        };
        let Some(action) = voice.action(intent) else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(backend) = &self.backend {
                    if let Some(media) = self
                        .message_snapshots
                        .get_mut(&message)
                        .and_then(|message| message.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                    }
                    backend.send(crate::backend::Command::Download {
                        chat,
                        message: message.clone(),
                    });
                    self.refresh_message_row(&message);
                }
            }
            crate::model::Action::PlayVoice { message, path } => {
                let voice_note = matches!(
                    self.message_snapshots
                        .get(&message)
                        .map(|message| &message.content),
                    Some(crate::model::Content::Audio {
                        voice_note: true,
                        ..
                    })
                );
                self.audio.media.set_speed(if voice_note {
                    self.settings.voice_speed
                } else {
                    1.0
                });
                self.audio
                    .audio_errors
                    .remove(&(audio_message.chat.clone(), message.clone()));
                let previous = self.audio.playing_audio.replace(message.clone());
                if let Err(error) = self.audio.media.toggle_playback(&message, &path) {
                    self.audio.audio_errors.insert(
                        (
                            self.active_chat.clone().unwrap_or_default(),
                            message.clone(),
                        ),
                        error,
                    );
                }
                if let Some(previous) = previous {
                    self.refresh_message_row(&previous);
                }
                self.refresh_message_row(&message);
                self.schedule_voice_poll(sender);
            }
            crate::model::Action::SeekVoice {
                message,
                path,
                fraction,
            } => {
                if matches!(
                    audio_message.content,
                    crate::model::Content::Audio {
                        voice_note: false,
                        ..
                    }
                ) {
                    self.audio.media.set_speed(1.0);
                }
                let previous = self.audio.playing_audio.replace(message.clone());
                if let Err(error) = self.audio.media.seek(&message, &path, fraction) {
                    self.audio.audio_errors.insert(
                        (
                            self.active_chat.clone().unwrap_or_default(),
                            message.clone(),
                        ),
                        error,
                    );
                }
                if let Some(previous) = previous {
                    self.refresh_message_row(&previous);
                }
                self.refresh_message_row(&message);
                self.schedule_voice_poll(sender);
            }
            crate::model::Action::CycleVoiceSpeed => {
                self.settings.voice_speed = self.audio.media.cycle_speed();
                if self.settings.save(&self.settings_path).is_err() {
                    self.status = "Could not save voice playback speed".into();
                }
                if let Some(id) = self.audio.playing_audio.clone() {
                    self.refresh_message_row(&id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn project_voice(
        &self,
        message: &crate::model::Message,
    ) -> Option<crate::native_voice::VoiceMessage> {
        let crate::model::Content::Audio {
            media,
            seconds,
            voice_note,
            waveform,
        } = &message.content
        else {
            return None;
        };
        if message.from_me && !voice_note {
            return None;
        }
        let mut projected = crate::native_voice::project(crate::native_voice::VoiceMessageInput {
            chat: &message.chat,
            message: &message.id,
            media,
            seconds: *seconds,
            waveform,
            generated_waveform: self
                .audio
                .audio_waveforms
                .get(&(message.chat.clone(), message.id.clone()))
                .map(Vec::as_slice)
                .or_else(|| self.audio.media.waveform(&message.id)),
            playback: self.audio.media.playback_status(&message.id),
            speed: if *voice_note {
                self.audio.media.speed()
            } else {
                1.0
            },
        });
        if let Some(error) = self
            .audio
            .audio_errors
            .get(&(message.chat.clone(), message.id.clone()))
        {
            projected.error = Some(error.clone());
        }
        Some(projected)
    }

    pub(super) fn refresh_selected_voice(&mut self) {
        self.audio.selected_voice = self
            .audio
            .selected_voice_message
            .as_ref()
            .and_then(|id| self.message_snapshots.get(id))
            .and_then(|message| self.project_voice(message));
        if self.audio.selected_voice.is_none() {
            self.audio.selected_voice_message = None;
        }
    }

    pub(super) fn activate_voice(&mut self, sender: &ComponentSender<Self>) {
        let Some(action) = self
            .audio
            .selected_voice
            .as_ref()
            .and_then(|voice| voice.action(crate::native_voice::VoiceIntent::Activate))
        else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(backend) = &self.backend {
                    if let Some(media) = self
                        .message_snapshots
                        .get_mut(&message)
                        .and_then(|message| message.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                    }
                    backend.send(crate::backend::Command::Download { chat, message });
                    self.status = "Downloading voice".into();
                    self.refresh_selected_voice();
                }
            }
            crate::model::Action::PlayVoice { message, path } => {
                if self.audio.media.toggle_playback(&message, &path).is_err() {
                    self.status = "Voice playback could not start.".into();
                } else if self.audio.media.is_playing() {
                    self.audio.playing_audio = Some(message.clone());
                }
                self.refresh_selected_voice();
                self.schedule_voice_poll(sender);
            }
            _ => {}
        }
    }

    pub(super) fn seek_voice(&mut self, fraction: f64, sender: &ComponentSender<Self>) {
        let Some(action) = self.audio.selected_voice.as_ref().and_then(|voice| {
            voice.action(crate::native_voice::VoiceIntent::Seek(fraction as f32))
        }) else {
            return;
        };
        if let crate::model::Action::SeekVoice {
            message,
            path,
            fraction,
        } = action
        {
            if self.audio.media.seek(&message, &path, fraction).is_err() {
                self.status = "Voice playback could not seek".into();
            } else {
                self.audio.playing_audio = Some(message.clone());
            }
            self.refresh_selected_voice();
            self.schedule_voice_poll(sender);
        }
    }

    pub(super) fn recording_action(&mut self, intent: crate::native_voice::RecordingIntent) {
        let active = self.audio.media.is_recording();
        let projection =
            crate::native_voice::project_recording(crate::native_voice::VoiceRecordingInput {
                recording: active,
                elapsed: self.audio.media.recording_elapsed().unwrap_or_default(),
                levels: &self.audio.media.recording_levels(),
            });
        let Some(action) = projection.action(intent) else {
            return;
        };
        match action {
            crate::model::Action::StartRecording => {
                if self.can_send_voice() {
                    self.audio.media.start_recording();
                }
            }
            crate::model::Action::CancelRecording => {
                self.audio.media.cancel_recording();
                self.status = "Recording canceled".into();
            }
            crate::model::Action::SendRecording => {
                let Some(samples) = self.audio.media.finish_recording() else {
                    return;
                };
                match samples {
                    Ok(samples) => {
                        if let (Some(chat), Some(backend)) =
                            (self.active_chat.clone(), self.backend.as_ref())
                        {
                            let quoting = self
                                .reply_to
                                .as_ref()
                                .filter(|(reply_chat, _)| reply_chat == &chat)
                                .map(|(_, id)| id.clone());
                            backend.send(crate::backend::Command::SendVoice {
                                chat,
                                samples,
                                quoting,
                            });
                            self.audio.voice_send_pending = true;
                            self.status = "Sending voice message".into();
                        }
                    }
                    Err(_) => self.status = "Could not record voice message".into(),
                }
            }
            _ => {}
        }
    }

    pub(super) fn poll_voice(&mut self, sender: &ComponentSender<Self>) {
        if let Err(error) = self.audio.media.poll() {
            self.status = "Audio playback could not continue.".into();
            if let (Some(chat), Some(id)) = (&self.active_chat, &self.audio.playing_audio) {
                self.audio
                    .audio_errors
                    .insert((chat.clone(), id.clone()), error);
            }
        }
        if let Some(id) = self.audio.playing_audio.clone() {
            if self.audio.media.actually_playing(&id) {
                self.tell_played(&id);
            }
            self.update_audio_row(&id);
        }
        self.refresh_selected_voice();
        if self.audio.media.is_recording() {
            self.audio
                .recording_meter
                .set_levels(&self.audio.media.recent_recording_levels(crate::voice::BARS));
        }
        self.schedule_voice_poll(sender);
    }

    fn tell_played(&mut self, id: &str) {
        let Some((chat, message)) = self
            .active_chat
            .as_ref()
            .zip(self.message_snapshots.get(id))
            .filter(|(_, message)| {
                !message.from_me
                    && matches!(
                        message.content,
                        crate::model::Content::Audio {
                            voice_note: true,
                            ..
                        }
                    )
            })
            .map(|(chat, message)| (chat.clone(), message.clone()))
        else {
            return;
        };
        if !self
            .audio
            .played_voice
            .insert((chat.clone(), message.id.clone()))
        {
            return;
        }
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::MarkPlayed {
                chat,
                message: message.id,
                sender: message.sender,
                receipts: self.settings.send_read_receipts && !self.account_receipts_off,
            });
        }
    }

    pub(super) fn cycle_voice_speed(&mut self) {
        let Some(action) = self
            .audio
            .selected_voice
            .as_ref()
            .and_then(|voice| voice.action(crate::native_voice::VoiceIntent::CycleSpeed))
        else {
            return;
        };
        if matches!(action, crate::model::Action::CycleVoiceSpeed) {
            self.settings.voice_speed = self.audio.media.cycle_speed();
            if let Err(_error) = self.settings.save(&self.settings_path) {
                self.status = "Could not save voice playback speed".into();
            }
            self.refresh_selected_voice();
        }
    }

    pub(super) fn schedule_voice_poll(&self, sender: &ComponentSender<Self>) {
        if self.audio.media.is_playing() || self.audio.media.is_recording() {
            let sender = sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(100), move || {
                sender.input(Input::PollVoice);
            });
        }
    }
}
