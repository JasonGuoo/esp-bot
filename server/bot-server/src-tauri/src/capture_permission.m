// Explicit app-level TCC consent before WKWebView's transient audio request.
// Official AVCaptureDevice authorization API; this adapter never captures media.
#import <AVFoundation/AVFoundation.h>
#include <stdbool.h>

int espbot_microphone_status(void) {
    switch ([AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]) {
        case AVAuthorizationStatusAuthorized: return 1;
        case AVAuthorizationStatusNotDetermined: return -1;
        default: return 0;
    }
}

void espbot_request_microphone(void (*done)(void *, bool), void *context) {
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) {
        done(context, granted);
    }];
}
