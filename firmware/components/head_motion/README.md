# head_motion — stationary adapter

The target configuration has no installed motor, IMU or joint feedback. `head_motion_get_status()` reports these as absent; measured angles are NAN with validity false. Requests return `ESP_ERR_NOT_SUPPORTED`; stop has no hardware side effect.

`app_main` uses `attention_controller_observe_stationary` and a NULL measured pose. It never substitutes a commanded or zero angle for real feedback. A future pan/tilt adapter must implement actual feedback, power/limit handling and stop before advertising capability. No build or hardware validation performed.
