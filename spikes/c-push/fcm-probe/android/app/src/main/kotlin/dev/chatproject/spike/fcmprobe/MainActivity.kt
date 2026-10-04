package dev.chatproject.spike.fcmprobe

import android.Manifest
import android.os.Build
import android.os.Bundle
import android.util.Log
import com.google.firebase.messaging.FirebaseMessaging
import io.flutter.embedding.android.FlutterActivity

class MainActivity : FlutterActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
        FirebaseMessaging.getInstance().token
            .addOnSuccessListener { saveToken(this, it) }
            .addOnFailureListener { Log.e(TAG, "getToken failed (DUMMY google-services.json? no Play services?)", it) }
    }
}
